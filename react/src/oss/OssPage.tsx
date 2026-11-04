//! 文档库(对象存储文件浏览 + markdown 编辑)—— rust/frontend/src/oss.rs 的 React 移植。
//! 路由 #/oss:列出桶的目录结构,markdown 文件在独立 Vditor 实例里编辑,
//! 保存直接写回桶,绕过 D1。S3 请求在浏览器内做 SigV4 签名(见 sigv4.ts)。
import { useCallback, useEffect, useRef, useState } from 'react';
import Vditor from 'vditor';
import { api } from '../api';
import { ExpandIcon, ShrinkIcon } from '../icons';
import { SuggestPanel, useSuggest } from '../suggest';
import { OSS_ENABLED } from './config';
import { createCommentTool, createFontColorTool } from './editorTools';
import { CommentDrawer, useComments } from './comments';
import { deleteObject, getObject, getObjectBytes, listAllEntriesUnder, objectExists, putObject } from './s3';
import { OssEntry, TreeNode, buildTree, emptyTree, findNode } from './tree';

const DEFAULT_FONT_SIZE = 16;
const MIN_FONT_SIZE = 10;
const MAX_FONT_SIZE = 40;
const OSS_FONT_STEP = 2;

/// 编辑器设置的 sidecar 文件:<key>.settings.json
const settingsKey = (key: string) => `${key}.settings.json`;

/// HTML5 拖放的负载类型:拖动文件树中的文件/文件夹(S3 无 rename,移动 =
/// 目标键 copy + 源键 delete,与重命名同一套 helper)
const OSS_DND_MIME = 'application/x-urlnote-oss';

function readDrag(e: React.DragEvent): { key: string; name: string; isDir: boolean } | null {
  try {
    const v = JSON.parse(e.dataTransfer.getData(OSS_DND_MIME));
    return typeof v?.key === 'string' && typeof v?.name === 'string' ? v : null;
  } catch {
    return null;
  }
}

/// 文件夹行/根目录行的投放目标处理器:dragover 高亮,drop 触发 onMove
function dropTargetProps(
  onMove: (key: string, name: string, isDir: boolean, targetPrefix: string) => void,
  targetPrefix: string,
) {
  return {
    onDragOver: (e: React.DragEvent) => {
      if (e.dataTransfer.types.indexOf(OSS_DND_MIME) === -1) return;
      e.preventDefault();
      e.stopPropagation();
      e.dataTransfer.dropEffect = 'move';
      e.currentTarget.classList.add('oss-drag-over');
    },
    onDragLeave: (e: React.DragEvent) => {
      e.currentTarget.classList.remove('oss-drag-over');
    },
    onDrop: (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      e.currentTarget.classList.remove('oss-drag-over');
      const p = readDrag(e);
      if (p) onMove(p.key, p.name, !!p.isDir, targetPrefix);
    },
  };
}

/// Go/Rust 后端同规则:<=0 用默认,取整后夹在 [10,40]
function resolveFontSize(n: number): number {
  if (!(n > 0)) return DEFAULT_FONT_SIZE;
  const r = Math.round(n);
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, r));
}

/// 与 shared::valid_color 一致
function validColor(c: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(c);
}

function currentTime(): string {
  return new Date().toTimeString().slice(0, 8);
}

/// 状态栏只显示消息本体,不带 "Error: " 前缀
function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function sanitizeName(raw: string): string | null {
  const name = [...raw]
    .filter((c) => !['/\\:*?"<>|\n\r'].includes(c))
    .slice(0, 120)
    .join('')
    .trim();
  return name && name !== '.' && name !== '..' ? name : null;
}

function fileKind(name: string): 'image' | 'pdf' | 'text' {
  const ext = name.split('.').pop()?.toLowerCase() || '';
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'avif'].includes(ext)) return 'image';
  if (ext === 'pdf') return 'pdf';
  return 'text';
}

function fileIcon(name: string): string {
  const kind = fileKind(name);
  return kind === 'image' ? '🖼️' : kind === 'pdf' ? '📕' : '📄';
}

function imageMime(name: string): string {
  const ext = name.split('.').pop()?.toLowerCase() || '';
  return (
    ({ png: 'image/png', gif: 'image/gif', webp: 'image/webp', avif: 'image/avif' } as Record<string, string>)[ext] ||
    'image/jpeg'
  );
}

interface PageState {
  tree: TreeNode;
  current: string | null;
  currentName: string;
  dirty: boolean;
  status: string;
  fontSize: number;
  color: string;
  previewUrl: string | null;
}

export function OssRoute() {
  if (!OSS_ENABLED) {
    return (
      <div className="oss-page">
        <div className="oss-top">
          <a className="oss-home" href="#/">
            📝 便签
          </a>
          <span className="oss-brand">🗂 文件库</span>
        </div>
        <div className="oss-empty">对象存储未配置：需要 VITE_OSS_AK / VITE_OSS_BUCKET 构建参数</div>
      </div>
    );
  }
  return <OssPage />;
}

function OssPage() {
  const hostRef = useRef<HTMLDivElement>(null);
  const vdRef = useRef<Vditor | null>(null);
  const suppressUntil = useRef(0);
  // 页面级状态走 ref + 版本号(与 Rust 的命令式信号同构:处理器多、互相接力,
  // 全量 setState 反而容易在异步回调里拿到过期值)
  const st = useRef<PageState>({
    tree: emptyTree(),
    current: null,
    currentName: '',
    dirty: false,
    status: '',
    fontSize: DEFAULT_FONT_SIZE,
    color: '',
    previewUrl: null,
  });
  const [, setVer] = useState(0);
  const bump = useCallback(() => setVer((v) => v + 1), []);

  const cmtRef = useRef<{ setContext(d: string): void; ensureOpen(d: string): void; addFromSelection(): void } | null>(null);

  const [lang, setLang] = useState(
    () => localStorage.getItem('urlnote-suggest-lang') || (import.meta as any).env.VITE_SUGGEST_LANG || '',
  );
  const [max, setMax] = useState(false);
  useEffect(() => {
    if (lang) localStorage.setItem('urlnote-suggest-lang', lang);
    else localStorage.removeItem('urlnote-suggest-lang');
  }, [lang]);

  // 最大化时 ESC 还原(词典面板打开时 ESC 先关面板,不退出最大化)
  useEffect(() => {
    if (!max) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setMax(false);
    };
    document.addEventListener('keydown', h);
    return () => document.removeEventListener('keydown', h);
  }, [max]);

  // 词典提示:接受词后文件变脏(与 Rust suggest_insert 一致)
  const suggest = useSuggest(
    true,
    lang,
    useCallback(() => {
      st.current.dirty = true;
      bump();
    }, [bump]),
  );

  const getRoot = useCallback(() => hostRef.current?.querySelector('.vditor-content') ?? null, []);
  const onHint = useCallback(
    (m: string) => {
      st.current.status = m;
      bump();
    },
    [bump],
  );
  const cmt = useComments(getRoot, onHint);
  cmtRef.current = { setContext: cmt.setContext, ensureOpen: cmt.ensureOpen, addFromSelection: cmt.addFromSelection };

  // ---- 树 ----

  const refreshTree = useCallback(() => {
    st.current.status = '加载目录…';
    bump();
    listAllEntriesUnder('')
      .then((entries) => {
        st.current.tree = buildTree(entries, st.current.tree);
        st.current.status = '';
        bump();
      })
      .catch((e) => {
        st.current.status = errMsg(e);
        bump();
      });
  }, [bump]);

  useEffect(() => {
    refreshTree();
  }, [refreshTree]);

  const toggleDir = (prefix: string) => {
    const node = findNode(st.current.tree, prefix);
    if (node) node.open = !node.open;
    bump();
  };

  // ---- 文件操作 ----

  const applySettings = (text: string) => {
    let v: { font_size?: unknown; color?: unknown };
    try {
      v = JSON.parse(text);
    } catch {
      return; // 404/损坏则保持默认
    }
    const s = st.current;
    s.fontSize = typeof v.font_size === 'number' ? resolveFontSize(v.font_size) : DEFAULT_FONT_SIZE;
    s.color = typeof v.color === 'string' && validColor(v.color) ? v.color : '';
    bump();
  };

  const openFile = (key: string, name: string) => {
    st.current.status = `载入 ${name}…`;
    bump();
    getObject(key)
      .then((text) => {
        const vd = vdRef.current;
        if (!vd) return;
        // Vditor fires input asynchronously right after setValue —
        // that echo is not a user edit
        suppressUntil.current = Date.now() + 500;
        vd.setValue(text);
        const s = st.current;
        s.current = key;
        s.currentName = name;
        s.dirty = false;
        s.fontSize = DEFAULT_FONT_SIZE;
        s.color = '';
        cmtRef.current?.setContext(`oss/${key}`);
        document.title = `${name} · 文件库`;
        s.status = `已载入 ${name}`;
        bump();
        getObject(settingsKey(key)).then(applySettings).catch(() => {});
      })
      .catch((e) => {
        st.current.status = errMsg(e);
        bump();
      });
  };

  const settingsJson = () => JSON.stringify({ font_size: st.current.fontSize, color: st.current.color });

  const saveCurrent = () => {
    const key = st.current.current;
    const vd = vdRef.current;
    if (!key || !vd) return;
    const content = vd.getValue();
    st.current.status = '保存中…';
    bump();
    putObject(key, content)
      .then(() =>
        // 编辑器设置随文件一起持久化,失败不阻塞主保存
        putObject(settingsKey(key), settingsJson()).catch(() => {}),
      )
      .then(() => {
        st.current.dirty = false;
        st.current.status = `已保存 ${currentTime()}`;
        bump();
        refreshTree();
      })
      .catch((e) => {
        st.current.status = errMsg(e);
        bump();
      });
  };

  /// Rebase the open-file state if `old` was renamed to `newPrefix`,
  /// or clear it when the file was deleted.
  const fixCurrentAfterMove = (old: string, newPrefix: string | null) => {
    const s = st.current;
    const cur = s.current;
    if (!cur) return;
    const affected = old.endsWith('/') ? cur.startsWith(old) : cur === old;
    if (!affected) return;
    if (newPrefix !== null) {
      const rest = cur.startsWith(old) ? cur.slice(old.length) : '';
      const newKey = `${newPrefix}${rest}`;
      s.current = newKey;
      s.currentName = newKey.split('/').pop() || newKey;
    } else {
      s.current = null;
      s.currentName = '';
      s.dirty = false;
      vdRef.current?.setValue('');
      document.title = '文件库';
    }
    bump();
  };

  /// Copy everything under oldPrefix to newPrefix, then delete the
  /// originals. S3 has no rename — GET+PUT per object (markdown scale).
  const copyPrefix = async (oldPrefix: string, newPrefix: string): Promise<void> => {
    const entries = await listAllEntriesUnder(oldPrefix);
    const total = entries.length;
    for (let i = 0; i < total; i++) {
      const e = entries[i];
      if (!e.key.startsWith(oldPrefix)) continue;
      const target = `${newPrefix}${e.key.slice(oldPrefix.length)}`;
      st.current.status = `重命名中 (${i + 1}/${total})…`;
      bump();
      const content = await getObject(e.key);
      await putObject(target, content);
      await deleteObject(e.key);
    }
    if (total === 0) {
      // folder had no objects at all — leave a marker so it still exists
      await putObject(newPrefix, '');
    }
  };

  const renameFileKey = async (oldKey: string, newKey: string): Promise<void> => {
    const content = await getObject(oldKey);
    await putObject(newKey, content);
    await deleteObject(oldKey);
    // 编辑器设置 sidecar 一并移动(best-effort)
    try {
      const sidecar = await getObject(settingsKey(oldKey));
      await putObject(settingsKey(newKey), sidecar);
      await deleteObject(settingsKey(oldKey));
    } catch {
      /* sidecar 不存在就跳过 */
    }
  };

  const createIn = (dirPrefix: string, isDir: boolean) => {
    const what = isDir ? '文件夹' : '文件';
    const raw = window.prompt(`在 ${dirPrefix} 下新建${what}：`);
    const name = raw !== null ? sanitizeName(raw) : null;
    if (!name) return;
    let key: string;
    let content: string;
    let finalName: string;
    if (isDir) {
      key = `${dirPrefix}${name}/`;
      content = '';
      finalName = name;
    } else {
      finalName = name.includes('.') ? name : `${name}.md`;
      key = `${dirPrefix}${finalName}`;
      content = `# ${finalName}\n`;
    }
    st.current.status = `创建 ${finalName}…`;
    bump();
    const run = async () => {
      try {
        if (!isDir && (await objectExists(key))) {
          if (!window.confirm(`${finalName} 已存在，覆盖它？`)) {
            st.current.status = '已取消';
            bump();
            return;
          }
        }
        await putObject(key, content);
        st.current.status = `已创建 ${finalName}`;
        bump();
        refreshTree();
        if (!isDir) openFile(key, finalName);
      } catch (e) {
        st.current.status = errMsg(e);
        bump();
      }
    };
    void run();
  };

  const renameEntry = (path: string, isDir: boolean) => {
    const oldName = path.replace(/\/+$/, '').split('/').pop() || '';
    const raw = window.prompt(`将 ${oldName} 重命名为：`);
    const name = raw !== null ? sanitizeName(raw) : null;
    if (!name) return;
    const trimmed = path.replace(/\/+$/, '');
    const parent = trimmed.includes('/') ? trimmed.slice(0, trimmed.lastIndexOf('/') + 1) : '';
    let newPath: string;
    let confirmMsg: string;
    if (isDir) {
      newPath = `${parent}${name}/`;
      confirmMsg = `文件夹内的所有文件将移动到 ${parent}${name}/，继续？`;
    } else {
      const finalName = name.includes('.') ? name : `${name}.md`;
      newPath = `${parent}${finalName}`;
      confirmMsg = `确定重命名为 ${finalName}？`;
    }
    if (newPath === path) return;
    if (!window.confirm(confirmMsg)) return;
    st.current.status = `重命名 ${oldName}…`;
    bump();
    const run = async () => {
      try {
        if (isDir) await copyPrefix(path, newPath);
        else await renameFileKey(path, newPath);
        fixCurrentAfterMove(path, newPath);
        st.current.status = `已重命名为 ${name}`;
        bump();
        refreshTree();
      } catch (e) {
        st.current.status = errMsg(e);
        bump();
      }
    };
    void run();
  };

  const deleteEntry = (path: string, isDir: boolean) => {
    const name = path.replace(/\/+$/, '').split('/').pop() || '';
    const hint = isDir ? '（仅限空文件夹）' : '';
    if (!window.confirm(`删除 ${name}${hint}？此操作不可恢复。`)) return;
    st.current.status = `删除 ${name}…`;
    bump();
    const run = async () => {
      try {
        if (isDir) {
          // only empty folders: anything besides the marker blocks it
          const entries = await listAllEntriesUnder(path);
          if (entries.some((e) => e.key !== path)) {
            throw new Error('文件夹非空，请先删除其中的文件');
          }
          await deleteObject(path);
        } else {
          await deleteObject(path);
          // 编辑器设置 sidecar 一并删除(best-effort)
          await deleteObject(settingsKey(path)).catch(() => {});
        }
        fixCurrentAfterMove(path, null);
        st.current.status = `已删除 ${name}`;
        bump();
        refreshTree();
      } catch (e) {
        st.current.status = errMsg(e);
        bump();
      }
    };
    void run();
  };

  // ---- 打开分发 ----

  /// 拖动移动:文件走 renameFileKey,文件夹走 copyPrefix;targetPrefix 为 '' = 根目录
  const moveEntry = (key: string, name: string, isDir: boolean, targetPrefix: string) => {
    const trimmed = key.replace(/\/+$/, '');
    const i = trimmed.lastIndexOf('/');
    const parent = i < 0 ? '' : trimmed.slice(0, i + 1);
    if (parent === targetPrefix) return; // 已在目标目录
    if (isDir && (targetPrefix === key || targetPrefix.startsWith(key))) return; // 不能拖进自己或自己的子树
    const newKey = isDir ? `${targetPrefix}${name}/` : `${targetPrefix}${name}`;
    const where = targetPrefix || '根目录';
    const confirmMsg = isDir
      ? `把文件夹 ${name} 移动到 ${where}?(其中所有文件一起移动)`
      : `把 ${name} 移动到 ${where}?`;
    if (!window.confirm(confirmMsg)) return;
    st.current.status = `移动 ${name}…`;
    bump();
    const run = async () => {
      try {
        // 目标已有同名对象时二次确认,避免拖动误覆盖
        const conflicts = await listAllEntriesUnder(newKey);
        if (
          conflicts.length > 0 &&
          !window.confirm(`目标已存在 ${conflicts.length} 个对象,同名将被覆盖,继续?`)
        ) {
          st.current.status = '已取消';
          bump();
          return;
        }
        if (isDir) await copyPrefix(key, newKey);
        else await renameFileKey(key, newKey);
        fixCurrentAfterMove(key, newKey);
        st.current.status = `已移动 ${name}`;
        bump();
        refreshTree();
      } catch (e) {
        st.current.status = errMsg(e);
        bump();
      }
    };
    void run();
  };

  /// 文件树点击分发:md → 编辑器;图片 → 预览浮层;PDF → 后端转 md 打开
  const openEntry = (key: string, name: string) => {
    const kind = fileKind(name);
    if (kind === 'image') previewImage(key, name);
    else if (kind === 'pdf') openPdfAsMd(key, name);
    else openFile(key, name);
  };

  const previewImage = (key: string, name: string) => {
    st.current.status = `加载 ${name}…`;
    bump();
    getObjectBytes(key)
      .then((bytes) => {
        const url = URL.createObjectURL(new Blob([bytes], { type: imageMime(name) }));
        if (st.current.previewUrl) URL.revokeObjectURL(st.current.previewUrl);
        st.current.previewUrl = url;
        st.current.status = '';
        bump();
      })
      .catch((e) => {
        st.current.status = errMsg(e);
        bump();
      });
  };

  const closePreview = () => {
    if (st.current.previewUrl) URL.revokeObjectURL(st.current.previewUrl);
    st.current.previewUrl = null;
    bump();
  };

  useEffect(
    () => () => {
      if (st.current.previewUrl) URL.revokeObjectURL(st.current.previewUrl);
    },
    [],
  );

  /// PDF → 后端转 markdown → 存为同名 .md(已存在则加序号)→ 在编辑器打开
  const openPdfAsMd = (key: string, name: string) => {
    st.current.status = `转换 ${name}…`;
    bump();
    const run = async () => {
      try {
        const bytes = await getObjectBytes(key);
        const form = new FormData();
        form.append('file', new Blob([bytes], { type: 'application/pdf' }));
        const out = await api.pdf2md(form);
        // 目标 key:去掉 .pdf 后缀 + .md;冲突时 -1/-2…
        let stem = key;
        if (stem.endsWith('.pdf')) stem = stem.slice(0, -4);
        else if (stem.endsWith('.PDF')) stem = stem.slice(0, -4);
        let target = `${stem}.md`;
        for (let i = 1; i < 50; i++) {
          if (!(await objectExists(target))) break;
          target = `${stem}-${i}.md`;
        }
        await putObject(target, out.markdown || '');
        const newName = target.split('/').pop() || target;
        st.current.status = `已转换 → ${newName}`;
        bump();
        openFile(target, newName);
        refreshTree();
      } catch (e) {
        st.current.status = errMsg(e);
        bump();
      }
    };
    void run();
  };

  // ---- 编辑器(整个页面生命周期一个实例) ----

  useEffect(() => {
    if (!hostRef.current || vdRef.current) return;
    const dark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    const vd = new Vditor(hostRef.current, {
      value: '',
      mode: 'wysiwyg',
      lang: 'zh_CN',
      cdn: '/vditor',
      theme: dark ? 'dark' : 'classic',
      // vditor 的 merge 会把字符串整体替换默认值,preview.theme 必须给对象
      // 才是它类型契约/运行时都认的写法(Rust 版传的字符串实际没有生效)
      preview: { theme: { current: dark ? 'dark' : 'light' } },
      placeholder: '从左侧选择一个 Markdown 文件开始编辑，编辑后点右上角保存…',
      cache: { enable: false },
      toolbar: [
        'headings',
        'bold',
        'italic',
        'strike',
        createFontColorTool(),
        createCommentTool(() => {
          const key = st.current.current;
          if (!key) return;
          cmtRef.current?.ensureOpen(`oss/${key}`);
          cmtRef.current?.addFromSelection();
        }),
        '|',
        'list',
        'ordered-list',
        'check',
        'quote',
        'line',
        'code',
        'inline-code',
        'link',
        'table',
        '|',
        'undo',
        'redo',
        '|',
        'edit-mode',
        'preview',
      ],
      input: () => {
        if (Date.now() < suppressUntil.current) {
          suggest.close();
          return;
        }
        st.current.dirty = true;
        bump();
      },
    });
    vdRef.current = vd;
    return () => {
      // Vditor 构造函数是异步初始化(this.vditor 在 i18n 等脚本加载完才挂上),
      // 初始化完成前调 destroy 会读 undefined.element 崩溃(StrictMode
      // 双挂载的清理阶段必现)——未完成的实例随 DOM 摘除丢弃即可
      if (vdRef.current && vdRef.current.vditor) vdRef.current.destroy();
      vdRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const s = st.current;
  const editorStyle = {
    '--oss-zoom': (s.fontSize / DEFAULT_FONT_SIZE).toFixed(3),
    ...(s.color ? { '--oss-color': s.color } : {}),
  } as React.CSSProperties;

  return (
    <div className="oss-page">
      <div className="oss-top">
        <a className="oss-home" href="#/" title="返回便签页面">
          📝 便签
        </a>
        <span className="oss-brand">🗂 文件库</span>
        <span className="oss-spacer" />
        <button type="button" className="tool" onClick={refreshTree} title="重新加载目录">
          🔄
        </button>
        <button
          type="button"
          className="tool oss-save"
          disabled={!(s.dirty && s.current)}
          onClick={saveCurrent}
          title="保存回对象存储"
        >
          💾 保存
        </button>
        <span className="oss-status">{s.status}</span>
      </div>
      <div className="oss-body">
        <aside className="oss-side">
          <div className="oss-tools">
            <button type="button" className="tool" onClick={() => createIn('', false)} title="在根目录新建 Markdown 文件">
              ＋ 文件
            </button>
            <button type="button" className="tool" onClick={() => createIn('', true)} title="在根目录新建文件夹">
              ＋ 文件夹
            </button>
          </div>
          <div className="oss-tree">
            <div
              className="oss-trow oss-root-drop"
              title="把文件/文件夹拖到此处移动到根目录"
              {...dropTargetProps(moveEntry, '')}
            >
              <span className="oss-ticon">📁</span>
              <span className="oss-tname">根目录</span>
            </div>
            {s.tree.dirs.length === 0 && s.tree.files.length === 0 && (
              <div className="oss-hint">（空桶，点上方按钮新建）</div>
            )}
            {s.tree.dirs.map((d) => (
              <TreeDir
                key={d.prefix}
                node={d}
                current={s.current}
                onToggle={toggleDir}
                onOpen={openEntry}
                onCreate={createIn}
                onRename={renameEntry}
                onDelete={deleteEntry}
                onMove={moveEntry}
              />
            ))}
            {s.tree.files
              .filter((f) => !f.name.endsWith('.settings.json'))
              .map((f) => (
                <FileRow
                  key={f.key}
                  entry={f}
                  current={s.current}
                  onOpen={openEntry}
                  onRename={renameEntry}
                  onDelete={deleteEntry}
                />
              ))}
          </div>
        </aside>
        <main className={'oss-main' + (max ? ' maximized' : '')}>
          <div className="oss-filebar">
            {s.currentName}
            {s.dirty && s.current && <span className="oss-dirty">● 未保存</span>}
            <span className="oss-spacer" />
            {s.current && (
              <>
                <select
                  className="tool lang-select"
                  title="提示语言"
                  value={lang}
                  onChange={(e) => setLang(e.target.value)}
                >
                  <option value="">自动</option>
                  {suggest.langs.map((l) => (
                    <option key={l.code} value={l.code}>
                      {l.name}
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  className="tool"
                  title="缩小显示"
                  onClick={() => {
                    st.current.fontSize = Math.max(MIN_FONT_SIZE, s.fontSize - OSS_FONT_STEP);
                    bump();
                  }}
                >
                  A−
                </button>
                <button
                  type="button"
                  className="tool"
                  title="放大显示"
                  onClick={() => {
                    st.current.fontSize = Math.min(MAX_FONT_SIZE, s.fontSize + OSS_FONT_STEP);
                    bump();
                  }}
                >
                  A+
                </button>
                {s.fontSize !== DEFAULT_FONT_SIZE && (
                  <button
                    type="button"
                    className="tool"
                    title="恢复默认大小"
                    onClick={() => {
                      st.current.fontSize = DEFAULT_FONT_SIZE;
                      bump();
                    }}
                  >
                    A
                  </button>
                )}
                <input
                  type="color"
                  className="tool color"
                  title="文字颜色"
                  value={s.color || '#000000'}
                  onChange={(e) => {
                    st.current.color = e.target.value;
                    bump();
                  }}
                />
                {s.color && (
                  <button
                    type="button"
                    className="tool"
                    title="恢复默认颜色"
                    onClick={() => {
                      st.current.color = '';
                      bump();
                    }}
                  >
                    默认
                  </button>
                )}
              </>
            )}
            <button
              type="button"
              className="tool oss-save"
              disabled={!(s.dirty && s.current)}
              onClick={saveCurrent}
              title="保存回对象存储"
            >
              💾 保存
            </button>
            <button
              type="button"
              className="tool"
              title={max ? '还原编辑区(Esc)' : '最大化编辑区'}
              onClick={() => setMax(!max)}
            >
              {max ? <ShrinkIcon /> : <ExpandIcon />}
            </button>
          </div>
          <div className="oss-editor" ref={hostRef} style={editorStyle} />
          <SuggestPanel state={suggest.state} onPick={suggest.accept} onHover={suggest.setActive} />
          <CommentDrawer ctl={cmt} />
          {max && (
            <div className="oss-floatbar">
              <button
                type="button"
                className="tool oss-save"
                disabled={!(s.dirty && s.current)}
                onClick={saveCurrent}
                title="保存回对象存储"
              >
                💾 保存
              </button>
              <span className="oss-status">{s.status}</span>
              <button type="button" className="tool" title="还原编辑区(Esc)" onClick={() => setMax(false)}>
                <ShrinkIcon />
              </button>
            </div>
          )}
          {s.previewUrl && (
            <div className="oss-lightbox" title="点击关闭" onClick={closePreview}>
              <img src={s.previewUrl} alt="preview" />
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

function TreeDir(props: {
  node: TreeNode;
  current: string | null;
  onToggle: (prefix: string) => void;
  onOpen: (key: string, name: string) => void;
  onCreate: (dirPrefix: string, isDir: boolean) => void;
  onRename: (path: string, isDir: boolean) => void;
  onDelete: (path: string, isDir: boolean) => void;
  onMove: (key: string, name: string, isDir: boolean, targetPrefix: string) => void;
}) {
  const { node } = props;
  return (
    <div className="oss-tdir">
      <div
        className="oss-trow"
        onClick={() => props.onToggle(node.prefix)}
        draggable
        onDragStart={(e) => {
          if ((e.target as HTMLElement).closest('button')) {
            e.preventDefault(); // 从操作按钮上拖起不算
            return;
          }
          e.dataTransfer.setData(
            OSS_DND_MIME,
            JSON.stringify({ key: node.prefix, name: node.name, isDir: true }),
          );
          e.dataTransfer.effectAllowed = 'move';
        }}
        {...dropTargetProps(props.onMove, node.prefix)}
      >
        <span className={'oss-chev' + (node.open ? ' oss-chev-open' : '')}>▸</span>
        <span className="oss-ticon">{node.open ? '📂' : '📁'}</span>
        <span className="oss-tname">{node.name}</span>
        <span className="oss-tact">
          <button
            type="button"
            title="在此新建文件"
            onClick={(ev) => {
              ev.stopPropagation();
              props.onCreate(node.prefix, false);
            }}
          >
            ＋📄
          </button>
          <button
            type="button"
            title="在此新建文件夹"
            onClick={(ev) => {
              ev.stopPropagation();
              props.onCreate(node.prefix, true);
            }}
          >
            ＋📁
          </button>
          <button
            type="button"
            title="重命名文件夹"
            onClick={(ev) => {
              ev.stopPropagation();
              props.onRename(node.prefix, true);
            }}
          >
            ✏️
          </button>
          <button
            type="button"
            title="删除文件夹（仅限空文件夹）"
            onClick={(ev) => {
              ev.stopPropagation();
              props.onDelete(node.prefix, true);
            }}
          >
            🗑
          </button>
        </span>
      </div>
      {node.open && (
        <div className="oss-tkids">
          {node.dirs.map((d) => (
            <TreeDir key={d.prefix} {...props} node={d} />
          ))}
          {node.files
            .filter((f) => !f.name.endsWith('.settings.json'))
            .map((f) => (
              <FileRow
                key={f.key}
                entry={f}
                current={props.current}
                onOpen={props.onOpen}
                onRename={props.onRename}
                onDelete={props.onDelete}
              />
            ))}
        </div>
      )}
    </div>
  );
}

function FileRow(props: {
  entry: OssEntry;
  current: string | null;
  onOpen: (key: string, name: string) => void;
  onRename: (path: string, isDir: boolean) => void;
  onDelete: (path: string, isDir: boolean) => void;
}) {
  const f = props.entry;
  return (
    <div
      className={'oss-trow oss-tfile' + (props.current === f.key ? ' oss-active' : '')}
      draggable
      onDragStart={(e) => {
        if ((e.target as HTMLElement).closest('button')) {
          e.preventDefault(); // 从操作按钮上拖起不算
          return;
        }
        e.dataTransfer.setData(
          OSS_DND_MIME,
          JSON.stringify({ key: f.key, name: f.name, isDir: false }),
        );
        e.dataTransfer.effectAllowed = 'move';
      }}
    >
      <button type="button" className="oss-open" onClick={() => props.onOpen(f.key, f.name)}>
        <span className="oss-ticon">{fileIcon(f.name)}</span>
        <span className="oss-tname">{f.name}</span>
      </button>
      <span className="oss-tact">
        <button type="button" title="重命名文件" onClick={() => props.onRename(f.key, false)}>
          ✏️
        </button>
        <button type="button" title="删除文件" onClick={() => props.onDelete(f.key, false)}>
          🗑
        </button>
      </span>
    </div>
  );
}
