package main

import "fmt"

func pageHTML(id string) string {
	return fmt.Sprintf(`<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>%s · urlnote</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  html, body { height: 100%%; margin: 0; }
  body {
    display: flex; flex-direction: column;
    font: 16px/1.7 -apple-system, "SF Mono", Menlo, Consolas, monospace;
    background: #fafafa; color: #222;
  }
  header {
    display: flex; align-items: center; gap: 12px;
    padding: 10px 16px; font-size: 13px;
    border-bottom: 1px solid #e2e2e2; color: #888;
    user-select: none;
  }
  header a { color: #888; text-decoration: none; }
  header a:hover { color: #444; }
  #status { margin-left: auto; transition: color .2s; }
  #status.saving { color: #d97706; }
  #status.saved  { color: #16a34a; }
  #status.error  { color: #dc2626; }
  textarea {
    flex: 1; width: 100%%; resize: none; border: 0; outline: none;
    padding: 20px 24px; background: transparent; color: inherit;
    font: inherit; tab-size: 4;
  }
  @media (prefers-color-scheme: dark) {
    body { background: #1b1b1b; color: #ddd; }
    header { border-color: #333; color: #999; }
    header a:hover { color: #ccc; }
  }
</style>
</head>
<body>
<header>
  <strong>urlnote</strong>
  <a href="/%[1]s" title="当前便签地址">/%[1]s</a>
  <a href="/" title="新建一个便签">＋ 新建</a>
  <span id="status"></span>
</header>
<textarea id="note" placeholder="在这里输入，内容会自动保存到这个网址…" autofocus spellcheck="false"></textarea>
<script>
(function () {
  var id = %[2]q;
  var ta = document.getElementById('note');
  var status = document.getElementById('status');
  var dirty = false;

  function setStatus(text, cls) {
    status.textContent = text;
    status.className = cls || '';
  }

  // load existing content
  fetch('/api/note/' + id)
    .then(function (r) { return r.json(); })
    .then(function (d) {
      if (!dirty) ta.value = d.content || '';
      setStatus(d.content ? '已加载' : '', '');
    })
    .catch(function () { setStatus('加载失败', 'error'); });

  // debounced auto-save
  var timer = null;
  ta.addEventListener('input', function () {
    dirty = true;
    setStatus('保存中…', 'saving');
    clearTimeout(timer);
    timer = setTimeout(save, 400);
  });

  function save() {
    fetch('/api/note/' + id, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content: ta.value })
    })
      .then(function (r) {
        if (!r.ok) throw new Error('http ' + r.status);
        setStatus('已保存 ' + new Date().toLocaleTimeString(), 'saved');
      })
      .catch(function () {
        setStatus('保存失败，将重试', 'error');
        timer = setTimeout(save, 2000);
      });
  }

  // save immediately when leaving the page
  window.addEventListener('beforeunload', function () {
    clearTimeout(timer);
    if (dirty) {
      navigator.sendBeacon('/api/note/' + id,
        new Blob([JSON.stringify({ content: ta.value })],
                 { type: 'application/json' }));
    }
  });
})();
</script>
</body>
</html>`, id, id)
}
