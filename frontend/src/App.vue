<template>
  <div v-if="showLogin" class="login-wrap">
    <form class="login-card" @submit.prevent="doLogin">
      <strong>urlnote</strong>
      <p class="login-hint">登录后开始编辑便签</p>
      <input
        v-model="loginUser"
        class="login-input"
        placeholder="用户名"
        autocomplete="username"
        required
      />
      <input
        v-model="loginPass"
        class="login-input"
        type="password"
        placeholder="密码"
        autocomplete="current-password"
        required
      />
      <button type="submit" class="login-btn">登 录</button>
      <span v-if="loginError" class="login-error">{{ loginError }}</span>
    </form>
  </div>
  <template v-else>
    <header>
      <strong>urlnote</strong>
      <a :href="'/' + id" title="当前便签地址">/{{ id }}</a>
      <a href="/" title="新建一个便签" @click.prevent="newNote">＋ 新建</a>
      <input
        v-model="title"
        class="title-input"
        placeholder="未命名便签"
        maxlength="200"
        title="便签标题"
      />
      <span v-if="author" class="author-badge" title="最后编辑者">✍️ {{ author }}</span>
      <span v-if="presenceOthers.length" class="presence-badge" :title="'正在查看本文：' + presenceOthers.join('、')">
        👀 {{ presenceOthers.join('、') }} 正在查看
      </span>
      <span class="tags-wrap" title="标签（逗号分隔，用于分类）">
        <span v-for="t in parsedTags" :key="t" class="chip">{{ t }}</span>
        <input v-model="tagsText" class="tags-input" placeholder="标签，逗号分隔" />
      </span>
      <span class="picker">
        <button type="button" class="tool" title="全部便签" @click.stop="togglePicker">☰ 便签</button>
        <div v-if="pickerOpen" class="picker-panel" @click.stop>
          <input
            v-model="pickerQuery"
            class="picker-search"
            placeholder="搜索标题、标签或 ID…"
          />
          <ul class="picker-list">
            <li v-for="n in filteredNotes" :key="n.id" @click="openNote(n.id)">
              <span class="row-main">
                <span class="row-title">{{ n.title || '(未命名)' }}</span>
                <span class="row-id">/{{ n.id }}</span>
              </span>
              <span v-if="!n.title && n.excerpt" class="row-excerpt">{{ n.excerpt }}</span>
              <span class="row-meta">
                <span v-for="t in n.tags" :key="t" class="chip">{{ t }}</span>
                <span v-if="n.author" class="row-author">✍️ {{ n.author }}</span>
                <span class="row-date">{{ (n.updated_at || '').slice(0, 10) }}</span>
              </span>
            </li>
            <li v-if="filteredNotes.length === 0" class="picker-empty">没有匹配的便签</li>
          </ul>
        </div>
      </span>
      <span class="tools">
        <button type="button" class="tool" title="缩小显示 (Ctrl+-)" @click="bumpFont(-FONT_STEP)">A−</button>
        <button type="button" class="tool" title="放大显示 (Ctrl+=)" @click="bumpFont(FONT_STEP)">A+</button>
        <button
          v-if="fontSize !== FONT_DEFAULT"
          type="button"
          class="tool"
          title="恢复默认大小 (Ctrl+0)"
          @click="fontSize = FONT_DEFAULT"
        >
          A
        </button>
        <input v-model="color" type="color" class="color" title="文字颜色" />
        <button v-if="color" type="button" class="tool" title="恢复默认颜色" @click="color = ''">默认</button>
      </span>
      <span class="user-wrap" title="当前用户">
        👤 {{ user?.username }}
        <button type="button" class="tool" @click="doLogout">退出</button>
      </span>
      <button type="button" class="tool save-btn" title="立即保存 (Ctrl+S)" @click="saveNow">💾 保存</button>
      <span id="status" :class="statusClass">{{ statusText }}</span>
    </header>
    <div v-if="remoteChanged" class="sync-banner">
      ⚠️ 此便签已被其他人更新
      <button type="button" @click="loadLatest">载入最新内容</button>
    </div>
    <div ref="editorEl" class="editor" :style="editorStyle"></div>
  </template>
</template>

<script setup>
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import Vditor from 'vditor'
import 'vditor/dist/index.css'

const alphabet = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789'
function newID() {
  const arr = new Uint8Array(5)
  crypto.getRandomValues(arr)
  return Array.from(arr, (b) => alphabet[b % alphabet.length]).join('')
}

// /:id is the note id; "/" redirects to a fresh one
let id = location.pathname.slice(1).split('/')[0]
if (!id || !/^[a-zA-Z0-9_-]{1,64}$/.test(id)) {
  id = newID()
  history.replaceState(null, '', '/' + id)
}
document.title = id + ' · urlnote'

// display settings — saved with the note, so a shared link shows identically.
// font_size scales the whole editor (zoom); color overrides the text color.
const FONT_DEFAULT = 16
const FONT_MIN = 10
const FONT_MAX = 40
const FONT_STEP = 2
const colorRe = /^#[0-9a-fA-F]{6}$/

function clampFont(n) {
  n = Math.round(Number(n))
  if (!Number.isFinite(n)) return FONT_DEFAULT
  return Math.min(FONT_MAX, Math.max(FONT_MIN, n))
}

const fontSize = ref(FONT_DEFAULT)
const color = ref('')
const title = ref('')
const tagsText = ref('')
const parsedTags = computed(() =>
  tagsText.value
    .split(/[,，]/)
    .map((t) => t.trim())
    .filter(Boolean),
)
const editorStyle = computed(() => ({
  '--note-zoom': fontSize.value / FONT_DEFAULT,
  ...(color.value ? { '--note-color': color.value } : {}),
}))

const statusText = ref('')
const statusClass = ref('')
let dirty = false
let loaded = false
let timer = null

// ---- auth ----
const user = ref(null)
const showLogin = ref(false)
const loginUser = ref('')
const loginPass = ref('')
const loginError = ref('')
const author = ref('')
const remoteUpdatedAt = ref('')
const remoteChanged = ref(false)
const presenceOthers = ref([])
let applyingRemote = false
let pollTimer = null
let presenceTimer = null
let autosaveTimer = null

function startCollaboration() {
  beatPresence()
  presenceTimer = setInterval(beatPresence, 15000)
  pollTimer = setInterval(pollNote, 10000)
  // autosave flushes pending edits every 3 minutes
  autosaveTimer = setInterval(() => {
    if (dirty && vditor) save()
  }, 180000)
}

function beatPresence() {
  fetch('/api/presence', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ note_id: id }),
  })
    .then((r) => (r.ok ? r.json() : null))
    .then((d) => {
      if (!d) return
      presenceOthers.value = (d.users || []).filter((u) => u !== user.value?.username)
    })
    .catch(() => {})
}

function pollNote() {
  if (applyingRemote || showLogin.value) return
  fetch('/api/note/' + id)
    .then((r) => {
      if (r.status === 401) {
        clearInterval(pollTimer)
        return null
      }
      return r.ok ? r.json() : null
    })
    .then((d) => {
      if (!d || !remoteUpdatedAt.value || d.updated_at === remoteUpdatedAt.value) return
      if (dirty) {
        // local unsaved edits — don't clobber them, offer to reload
        remoteChanged.value = true
      } else {
        applyRemote(d)
        setStatus('已同步 ' + (d.author || '他人') + ' 的更新', 'saved')
      }
    })
    .catch(() => {})
}

function applyRemote(d) {
  applyingRemote = true
  try {
    if (vditor) vditor.setValue(d.content || '')
    fontSize.value = clampFont(d.font_size || FONT_DEFAULT)
    color.value = colorRe.test(d.color || '') ? d.color : ''
    title.value = d.title || ''
    tagsText.value = (d.tags || []).join(', ')
    author.value = d.author || ''
  } finally {
    applyingRemote = false
  }
  remoteUpdatedAt.value = d.updated_at || ''
  remoteChanged.value = false
  dirty = false
  loaded = true
}

function loadLatest() {
  fetch('/api/note/' + id)
    .then((r) => (r.ok ? r.json() : null))
    .then((d) => {
      if (!d) return
      applyRemote(d)
      setStatus('已载入最新内容', 'saved')
    })
    .catch(() => setStatus('加载失败', 'error'))
}

function loadNote() {
  if (vditor) return
  fetch('/api/note/' + id)
    .then((r) => {
      if (r.status === 401) {
        showLogin.value = true
        return null
      }
      return r.json()
    })
    .then((d) => {
      if (!d) return
      fontSize.value = clampFont(d.font_size || FONT_DEFAULT)
      color.value = colorRe.test(d.color || '') ? d.color : ''
      title.value = d.title || ''
      tagsText.value = (d.tags || []).join(', ')
      author.value = d.author || ''
      remoteUpdatedAt.value = d.updated_at || ''
      setStatus(d.content ? '已加载' : '')
      loaded = true
      initEditor(d.content || '')
      startCollaboration()
    })
    .catch(() => setStatus('加载失败', 'error'))
}

function doLogin() {
  loginError.value = ''
  fetch('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: loginUser.value, password: loginPass.value }),
  })
    .then(async (r) => {
      if (!r.ok) {
        const d = await r.json().catch(() => null)
        loginError.value = d?.error || '登录失败'
        return
      }
      user.value = { username: loginUser.value }
      showLogin.value = false
      loadNote()
    })
    .catch(() => {
      loginError.value = '网络错误，请重试'
    })
}

function doLogout() {
  fetch('/api/logout', { method: 'POST' }).finally(() => location.reload())
}

onMounted(() => {
  fetch('/api/whoami')
    .then((r) => (r.ok ? r.json() : null))
    .then((d) => {
      if (!d) {
        showLogin.value = true
        return
      }
      user.value = d.user
      loadNote()
    })
    .catch(() => {
      showLogin.value = true
    })
})

function setStatus(text, cls = '') {
  statusText.value = text
  statusClass.value = cls
}

function bumpFont(delta) {
  fontSize.value = clampFont(fontSize.value + delta)
}

function notePayload() {
  return JSON.stringify({
    content: vditor ? vditor.getValue() : '',
    font_size: fontSize.value,
    color: color.value,
    title: title.value,
    tags: parsedTags.value,
  })
}

function save() {
  fetch('/api/note/' + id, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: notePayload(),
  })
    .then((r) => {
      if (r.status === 401) {
        location.reload() // session expired — back to the login page
        return null
      }
      if (!r.ok) throw new Error('http ' + r.status)
      return r.json()
    })
    .then((d) => {
      if (!d) return
      // the server echoes the stored payload — sync our change-detection clock
      remoteUpdatedAt.value = d.updated_at || remoteUpdatedAt.value
      author.value = d.author || author.value
      remoteChanged.value = false
      dirty = false
      setStatus('已保存 ' + new Date().toLocaleTimeString(), 'saved')
    })
    .catch(() => {
      setStatus('保存失败，将重试', 'error')
      timer = setTimeout(save, 2000)
    })
}

// mark edits as pending; the 3-minute autosave timer (and the save button /
// Ctrl+S) flush them — typing alone no longer triggers rapid saves
function markDirty() {
  dirty = true
  setStatus('有未保存的更改', 'saving')
}

function saveNow() {
  if (!dirty) {
    setStatus('没有更改需要保存')
    return
  }
  clearTimeout(timer)
  save()
}

const darkMode = window.matchMedia('(prefers-color-scheme: dark)')
let vditor = null
const editorEl = ref(null)

function initEditor(markdown) {
  vditor = new Vditor(editorEl.value, {
    value: markdown,
    // MarkText-style WYSIWYG: rendered like a preview, click anywhere to edit
    mode: 'wysiwyg',
    lang: 'zh_CN',
    // serve Vditor's runtime assets (icons, hljs themes, content themes)
    // from our own static assets instead of unpkg
    cdn: '/vditor',
    theme: darkMode.matches ? 'dark' : 'classic',
    preview: { theme: darkMode.matches ? 'dark' : 'light' },
    placeholder: '在这里输入 Markdown，内容会自动保存到这个网址…',
    // note content lives in D1 — no localStorage draft cache
    cache: { enable: false },
    // toolbar image button / paste / drag-drop upload to the worker, which
    // stores the image in KV and returns a URL to insert
    upload: {
      url: '/api/upload',
      max: 8 * 1024 * 1024,
    },
    toolbar: [
      'headings', 'bold', 'italic', 'strike', '|',
      'list', 'ordered-list', 'check', 'quote', 'line', 'code', 'inline-code', 'link', 'upload', 'table', '|',
      'undo', 'redo', '|',
      // switch to 即时渲染 / 分屏编辑 for raw markdown source; toggle 预览
      'edit-mode', 'preview',
    ],
    input() {
      if (!applyingRemote) markDirty()
    },
    after() {
      vditor.focus()
    },
  })
}

watch([fontSize, color, title, tagsText], () => {
  if (!loaded || applyingRemote) return
  markDirty()
})

// ---- searchable "all notes" dropdown ----
const pickerOpen = ref(false)
const pickerQuery = ref('')
const allNotes = ref([])

function togglePicker() {
  pickerOpen.value = !pickerOpen.value
  if (pickerOpen.value) {
    pickerQuery.value = ''
    fetch('/api/notes')
      .then((r) => r.json())
      .then((d) => {
        allNotes.value = d.notes || []
      })
      .catch(() => setStatus('便签列表加载失败', 'error'))
  }
}

const filteredNotes = computed(() => {
  const q = pickerQuery.value.trim().toLowerCase()
  if (!q) return allNotes.value
  return allNotes.value.filter(
    (n) =>
      (n.title || '').toLowerCase().includes(q) ||
      n.id.toLowerCase().includes(q) ||
      (n.tags || []).some((t) => t.toLowerCase().includes(q)),
  )
})

function openNote(nid) {
  pickerOpen.value = false
  location.href = '/' + nid
}

function closePicker(e) {
  if (!e.target.closest('.picker')) pickerOpen.value = false
}
window.addEventListener('click', closePicker)

// keep the editor theme in sync with the OS appearance
function onDarkModeChange(e) {
  vditor?.setTheme(e.matches ? 'dark' : 'classic')
}
darkMode.addEventListener('change', onDarkModeChange)

// Ctrl/Cmd +/- scales the editor, Ctrl/Cmd 0 resets it, Ctrl/Cmd S saves
function onKeydown(e) {
  if (!(e.ctrlKey || e.metaKey)) return
  if (e.key === 's' || e.key === 'S') {
    e.preventDefault()
    saveNow()
  } else if (e.key === '=' || e.key === '+') {
    e.preventDefault()
    bumpFont(FONT_STEP)
  } else if (e.key === '-') {
    e.preventDefault()
    bumpFont(-FONT_STEP)
  } else if (e.key === '0') {
    e.preventDefault()
    fontSize.value = FONT_DEFAULT
  }
}
window.addEventListener('keydown', onKeydown)

// save immediately when leaving the page
function onUnload() {
  clearTimeout(timer)
  if (dirty && vditor) {
    navigator.sendBeacon(
      '/api/note/' + id,
      new Blob([notePayload()], {
        type: 'application/json',
      }),
    )
  }
}
window.addEventListener('beforeunload', onUnload)

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('beforeunload', onUnload)
  window.removeEventListener('click', closePicker)
  darkMode.removeEventListener('change', onDarkModeChange)
  clearInterval(pollTimer)
  clearInterval(presenceTimer)
  clearInterval(autosaveTimer)
  vditor?.destroy()
})

function newNote() {
  location.href = '/' + newID()
}
</script>

<style>
:root {
  color-scheme: light dark;
}
* {
  box-sizing: border-box;
}
html,
body,
#app {
  height: 100%;
  margin: 0;
}
#app {
  display: flex;
  flex-direction: column;
  font: 16px/1.7 -apple-system, 'SF Mono', Menlo, Consolas, monospace;
  background: #fafafa;
  color: #222;
}
header {
  position: relative;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  padding: 10px 16px;
  font-size: 13px;
  border-bottom: 1px solid #e2e2e2;
  color: #888;
  user-select: none;
}
header a {
  color: #888;
  text-decoration: none;
}
header a:hover {
  color: #444;
}

.title-input,
.tags-input {
  font: inherit;
  font-size: 13px;
  color: #444;
  background: transparent;
  border: none;
  border-bottom: 1px dashed transparent;
  padding: 1px 2px;
  min-width: 60px;
}
.title-input {
  width: 180px;
}
.title-input:hover,
.title-input:focus,
.tags-input:hover,
.tags-input:focus {
  border-bottom-color: #bbb;
  outline: none;
}
.tags-wrap {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}
.tags-input {
  width: 110px;
}
.chip {
  display: inline-block;
  font-size: 11px;
  line-height: 1.5;
  padding: 0 7px;
  border-radius: 999px;
  background: #eef2f7;
  color: #476582;
  white-space: nowrap;
}

.picker {
  position: relative;
}
.picker-panel {
  position: absolute;
  top: calc(100% + 8px);
  left: 0;
  z-index: 100;
  width: 340px;
  max-height: 60vh;
  display: flex;
  flex-direction: column;
  background: #fff;
  border: 1px solid #e2e2e2;
  border-radius: 10px;
  box-shadow: 0 8px 30px rgba(0, 0, 0, 0.12);
  overflow: hidden;
  color: #333;
  user-select: none;
}
.picker-search {
  font: inherit;
  font-size: 13px;
  padding: 9px 12px;
  border: none;
  border-bottom: 1px solid #eee;
  outline: none;
  background: transparent;
  color: inherit;
}
.picker-list {
  list-style: none;
  margin: 0;
  padding: 4px;
  overflow-y: auto;
}
.picker-list li {
  padding: 7px 10px;
  border-radius: 7px;
  cursor: pointer;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.picker-list li:hover {
  background: #f3f6fa;
}
.row-main {
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
}
.row-title {
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row-id {
  color: #999;
  font-size: 11px;
  flex-shrink: 0;
}
.row-excerpt {
  font-size: 12px;
  color: #888;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row-meta {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}
.row-date {
  margin-left: auto;
  color: #aaa;
  font-size: 11px;
}
.picker-empty {
  color: #999;
  text-align: center;
  cursor: default !important;
}

.user-wrap {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
}
.save-btn {
  font-weight: 600;
}
.save-btn:hover {
  color: #16a34a !important;
  border-color: #16a34a !important;
}
.author-badge {
  font-size: 12px;
  color: #999;
  white-space: nowrap;
}
.presence-badge {
  font-size: 12px;
  color: #16a34a;
  white-space: nowrap;
}
.sync-banner {
  padding: 8px 16px;
  font-size: 13px;
  color: #b45309;
  background: #fef3c7;
  border-bottom: 1px solid #fde68a;
  display: flex;
  align-items: center;
  gap: 12px;
}
.sync-banner button {
  font: inherit;
  font-size: 12px;
  padding: 2px 10px;
  border: 1px solid #d97706;
  border-radius: 6px;
  background: transparent;
  color: #b45309;
  cursor: pointer;
}
.sync-banner button:hover {
  background: #d97706;
  color: #fff;
}

/* ---- login page ---- */
.login-wrap {
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #fafafa;
}
.login-card {
  display: flex;
  flex-direction: column;
  gap: 12px;
  width: 300px;
  padding: 36px 32px;
  background: #fff;
  border: 1px solid #e2e2e2;
  border-radius: 14px;
  box-shadow: 0 10px 40px rgba(0, 0, 0, 0.06);
  font-size: 14px;
}
.login-card strong {
  font-size: 18px;
  color: #222;
}
.login-hint {
  margin: -6px 0 4px;
  font-size: 12px;
  color: #999;
}
.login-input {
  font: inherit;
  padding: 9px 12px;
  border: 1px solid #ddd;
  border-radius: 8px;
  outline: none;
  background: transparent;
  color: inherit;
}
.login-input:focus {
  border-color: #888;
}
.login-btn {
  font: inherit;
  padding: 9px 0;
  border: none;
  border-radius: 8px;
  background: #222;
  color: #fff;
  cursor: pointer;
}
.login-btn:hover {
  background: #000;
}
.login-error {
  font-size: 12px;
  color: #dc2626;
  text-align: center;
}

.tools {
  display: flex;
  align-items: center;
  gap: 6px;
}
.tool,
.color {
  font: inherit;
  font-size: 13px;
  line-height: 1.4;
  color: #888;
  background: transparent;
  border: 1px solid #e2e2e2;
  border-radius: 6px;
  cursor: pointer;
}
.tool {
  padding: 1px 8px;
}
.tool:hover {
  color: #444;
  border-color: #bbb;
}
.color {
  width: 30px;
  height: 24px;
  padding: 0 2px;
}
#status {
  margin-left: auto;
  transition: color 0.2s;
}
#status.saving {
  color: #d97706;
}
#status.saved {
  color: #16a34a;
}
#status.error {
  color: #dc2626;
}

/* Vditor 4 mounts onto .editor itself (it gains the .vditor class), fills the
   remaining viewport and scrolls internally */
.editor {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}
.editor.vditor {
  height: auto;
  border: none;
  font-family: -apple-system, 'SF Mono', Menlo, Consolas, monospace;
  color: var(--note-color, inherit);
}
.editor .vditor-content {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  zoom: var(--note-zoom, 1);
}
/* --note-color (when set) recolors the text. Vditor 4 wraps every block in
   <pre class="vditor-reset"> (color: #24292e), so text blocks must opt back
   into the container color; pre>code (code blocks) keeps its own colors —
   syntax highlighting is untouched. */
.editor :is(.vditor-wysiwyg, .vditor-preview) pre.vditor-reset:not(:has(> code)) {
  color: inherit;
}

@media (prefers-color-scheme: dark) {
  #app {
    background: #1b1b1b;
    color: #ddd;
  }
  header {
    border-color: #333;
    color: #999;
  }
  header a:hover {
    color: #ccc;
  }
  .title-input,
  .tags-input {
    color: #ccc;
  }
  .title-input:hover,
  .title-input:focus,
  .tags-input:hover,
  .tags-input:focus {
    border-bottom-color: #555;
  }
  .chip {
    background: #2d333b;
    color: #9db3c8;
  }
  .picker-panel {
    background: #23272e;
    border-color: #333;
    color: #ddd;
    box-shadow: 0 8px 30px rgba(0, 0, 0, 0.5);
  }
  .picker-search {
    border-bottom-color: #333;
  }
  .picker-list li:hover {
    background: #2d333b;
  }
  .tool,
  .color {
    color: #999;
    border-color: #333;
  }
  .tool:hover {
    color: #ccc;
    border-color: #555;
  }
  .author-badge {
    color: #777;
  }
  .login-wrap {
    background: #1b1b1b;
  }
  .login-card {
    background: #23272e;
    border-color: #333;
    color: #ddd;
  }
  .login-card strong {
    color: #eee;
  }
  .login-hint {
    color: #888;
  }
  .login-input {
    border-color: #444;
    color: #ddd;
  }
  .login-input:focus {
    border-color: #777;
  }
  .login-btn {
    background: #ddd;
    color: #222;
  }
  .login-btn:hover {
    background: #fff;
  }
}
</style>
