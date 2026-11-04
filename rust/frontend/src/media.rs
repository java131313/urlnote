//! 媒体库(/media):左侧 S3 文件树(只显示 mp3/mp4 与文件夹),右侧
//! ffmpeg.wasm 全量转码后的 <video>/<audio> 播放器。与 react/src/media
//! 同设计:流走 /api/media/stream(后端 SigV4 代理 S3),转码产物按 key
//! 进 Cache API。播放器本体是注入的 JS 模块(window.__urlnote_media_*),
//! 进度/状态直接写 DOM,避免与 Leptos 信号桥接。

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use web_sys::RequestCredentials;

use crate::oss::{self, OssEntry, OSS_ENABLED};
use crate::{document, window, BASE_PATH};

#[derive(Clone, Default)]
struct MTree {
    prefix: String,
    name: String,
    open: bool,
    dirs: Vec<MTree>,
    files: Vec<OssEntry>,
}

fn is_media(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".mp3") || lower.ends_with(".mp4")
}

fn is_audio(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".mp3")
}

fn fmt_size(n: u64) -> String {
    if n < 1024 * 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

/// 平铺 LIST 结果 → 嵌套树(目录按其 prefix 归位)
fn build_tree(entries: &[OssEntry]) -> MTree {
    let mut root = MTree::default();
    for e in entries {
        let is_dir = e.key.ends_with('/');
        let parts: Vec<&str> = e.key.split('/').filter(|p| !p.is_empty()).collect();
        let mut cur = &mut root;
        for (i, part) in parts.iter().enumerate() {
            let last = i == parts.len() - 1;
            if last && !is_dir {
                if is_media(&e.name) {
                    cur.files.push(e.clone());
                }
                break;
            }
            let prefix = parts[..=i].join("/") + "/";
            let idx = match cur.dirs.iter().position(|d| d.prefix == prefix) {
                Some(i) => i,
                None => {
                    cur.dirs.push(MTree {
                        prefix: prefix.clone(),
                        name: part.to_string(),
                        open: false,
                        dirs: vec![],
                        files: vec![],
                    });
                    cur.dirs.len() - 1
                }
            };
            cur = &mut cur.dirs[idx];
        }
    }
    root
}

fn has_media(t: &MTree) -> bool {
    !t.files.is_empty() || t.dirs.iter().any(has_media)
}

// ---- 播放器 JS(注入一次;__BASE__ 在注入时替换为 BASE_PATH) ----

const PLAYER_JS: &str = r#"
let ffmpeg = null, loading = null;
const CACHE = 'urlnote-media-v1';
const ckey = (key) => location.origin + '/media-transcode/' + encodeURIComponent(key);
function setStatus(t) { const el = document.getElementById('media-status-text'); if (el) el.textContent = t; }
function setBar(p) {
  const w = document.getElementById('media-progress');
  if (w) w.style.display = p === null ? 'none' : '';
  const b = document.getElementById('media-progress-bar');
  if (b) b.style.width = (p ?? 0) + '%';
}
async function engine() {
  if (ffmpeg) return ffmpeg;
  loading ??= (async () => {
    setStatus('首次使用:正在加载 ffmpeg 核心(约 31MB)…'); setBar(8);
    const { FFmpeg } = await import('__BASE__/ffmpeg/ffmpeg/index.js');
    const f = new FFmpeg();
    await f.load({
      coreURL: '__BASE__/ffmpeg/core/ffmpeg-core.js',
      wasmURL: '__BASE__/ffmpeg/core/ffmpeg-core.wasm',
    });
    ffmpeg = f;
    return f;
  })();
  return loading;
}
window.__urlnote_media_play = async (url, isAudio, key) => {
  const stage = document.getElementById('media-stage-player');
  if (!stage) return;
  stage.innerHTML = '';
  setBar(0);
  try {
    let blob = null;
    try {
      const r = await (await caches.open(CACHE)).match(ckey(key));
      if (r) blob = await r.blob();
    } catch {}
    if (!blob) {
      const f = await engine();
      setStatus('下载中…'); setBar(5);
      const resp = await fetch(url, { credentials: 'include' });
      if (!resp.ok) throw new Error('下载失败 HTTP ' + resp.status);
      const data = new Uint8Array(await resp.arrayBuffer());
      const inName = 'in-' + key.split('/').pop();
      const outName = isAudio ? 'out.mp3' : 'out.mp4';
      await f.writeFile(inName, data);
      const onProg = ({ progress }) => {
        const p = Math.min(99, Math.round(progress * 100));
        setStatus('全量转码中 ' + p + '%…'); setBar(p);
      };
      f.on('progress', onProg);
      const code = await f.exec(isAudio
        ? ['-i', inName, '-vn', '-c:a', 'libmp3lame', '-b:a', '192k', outName]
        : ['-i', inName, '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '23',
           '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-b:a', '128k', '-movflags', 'faststart', outName]);
      f.off('progress', onProg);
      if (code !== 0) throw new Error('转码失败 (ffmpeg exit ' + code + ')');
      const out = await f.readFile(outName);
      blob = new Blob([out.buffer], { type: isAudio ? 'audio/mpeg' : 'video/mp4' });
      try { await f.deleteFile(inName); await f.deleteFile(outName); } catch {}
      try { await (await caches.open(CACHE)).put(ckey(key), new Response(blob)); } catch {}
    } else {
      setStatus('缓存命中,直接播放');
    }
    const u = URL.createObjectURL(blob);
    const m = document.createElement(isAudio ? 'audio' : 'video');
    m.controls = true; m.autoplay = true; m.src = u;
    m.className = isAudio ? 'media-audio' : 'media-video';
    stage.appendChild(m);
    setBar(null); setStatus('');
  } catch (e) {
    setStatus('⚠️ ' + (e && e.message ? e.message : String(e)));
    setBar(null);
  }
};
window.__urlnote_media_clear = async () => {
  await caches.delete(CACHE);
  setStatus('已清空转码缓存');
};
"#;

fn ensure_player_js() {
    if document()
        .get_element_by_id("urlnote-media-player-js")
        .is_some()
    {
        return;
    }
    let el = document().create_element("script").expect("script");
    el.set_id("urlnote-media-player-js");
    el.set_attribute("type", "module")
        .expect("script type");
    el.set_text_content(Some(&PLAYER_JS.replace("__BASE__", BASE_PATH)));
    document().head().expect("head").append_child(&el).expect("append script");
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = __urlnote_media_play)]
    fn media_play(url: &str, is_audio: bool, key: &str);
    #[wasm_bindgen(js_namespace = window, js_name = __urlnote_media_clear)]
    fn media_clear();
}

// ---- 页面 ----

/// /media 入口:未配置 → 提示页;未登录 → 登录表单(与 /oss 同门)
#[component]
pub fn MediaGate() -> impl IntoView {
    if !OSS_ENABLED {
        return view! {
            <div class="media-page">
                <div class="oss-top">
                    <a class="oss-home" href=format!("{BASE_PATH}/")>"📝 便签"</a>
                    <span class="oss-brand">"🎬 媒体库"</span>
                </div>
                <div class="oss-empty">"对象存储未配置:需要 OSS_AK / OSS_BUCKET 构建参数"</div>
            </div>
        }
        .into_any();
    }
    let checked = RwSignal::new(false);
    let show_login = RwSignal::new(false);
    spawn_local(async move {
        match gloo_net::http::Request::get(&crate::api_url("/api/whoami"))
            .credentials(RequestCredentials::Include)
            .send()
            .await
        {
            Ok(resp) if resp.ok() => {
                if let Ok(d) = resp.json::<shared::WhoamiResponse>().await {
                    crate::USER.with(|u| u.set(Some(d.user.username)));
                }
            }
            _ => show_login.set(true),
        }
        checked.set(true);
    });
    let on_ok = Callback::new(move |_| {
        let _ = window().location().reload();
    });
    view! {
        {move || {
            if !checked.get() {
                view! { <div class="boot">"加载中…"</div> }.into_any()
            } else if show_login.get() {
                view! { <crate::LoginView show_login on_ok/> }.into_any()
            } else {
                view! { <MediaView/> }.into_any()
            }
        }}
    }
    .into_any()
}

#[component]
fn MediaView() -> impl IntoView {
    let tree = RwSignal::new(MTree::default());
    let status = RwSignal::new(String::new());
    let current = RwSignal::new(None::<(String, String)>); // (key, name)

    let refresh = move || {
        status.set("加载目录…".to_string());
        spawn_local(async move {
            match oss::list_all_entries_under("").await {
                Ok(entries) => {
                    tree.set(build_tree(&entries));
                    status.set(String::new());
                }
                Err(e) => status.set(e),
            }
        });
    };
    Effect::new(move |_| refresh());

    let play = move |arg: (String, String)| {
        let (key, name) = arg;
        let stage = document().get_element_by_id("media-stage-player");
        if let Some(s) = stage {
            s.set_inner_html("");
        }
        current.set(Some((key.clone(), name.clone())));
        ensure_player_js();
        let path = format!("/api/media/stream/{}", encode_key(&key));
        media_play(&crate::api_url(&path), is_audio(&name), &key);
    };

    let on_play = Callback::new(play);

    view! {
        <div class="media-page">
            <div class="oss-top">
                <a class="oss-home" href=format!("{BASE_PATH}/") title="返回便签页面">"📝 便签"</a>
                <span class="oss-brand">"🎬 媒体库"</span>
                <span class="oss-spacer"></span>
                <button type="button" class="tool" on:click=move |_| refresh() title="重新加载目录">
                    "🔄"
                </button>
                <button type="button" class="tool" title="清空转码缓存(下次播放重新转码)"
                    on:click=move |_| { ensure_player_js(); media_clear(); }>
                    "🧹 清缓存"
                </button>
                <span class="oss-status">{move || status.get()}</span>
            </div>
            <div class="media-body">
                <aside class="media-side">
                    <div class="oss-tree">
                        {move || {
                            let t = tree.get();
                            if t.dirs.is_empty() && t.files.is_empty() {
                                view! { <div class="oss-hint">"(空桶,把 mp3/mp4 放进对象存储后点上方刷新)"</div> }.into_any()
                            } else {
                                view! {
                                    <span style="display:none"></span>
                                }.into_any()
                            }
                        }}
                        {move || {
                            let t = tree.get();
                            let cur = current.get();
                            t.dirs.iter().map(|d| view! {
                                <MDir node=d.clone() current=cur.clone() on_play=on_play/>
                            }).collect_view()
                        }}
                        {move || {
                            let t = tree.get();
                            let cur = current.get();
                            t.files.iter().map(|f| view! {
                                <MRow key=f.key.clone() name=f.name.clone() size=f.size active=cur.as_ref().map(|c| c.0 == f.key).unwrap_or(false) on_play=on_play/>
                            }).collect_view()
                        }}
                    </div>
                </aside>
                <main class="media-main">
                    <div class="media-filebar">
                        {move || current.get().map(|(_, name)| view! { <span class="media-title">{name}</span> })}
                    </div>
                    <div class="media-stage">
                        <div id="media-stage-player"></div>
                        <div class="media-hint" id="media-status-text"></div>
                        <div class="media-progress" id="media-progress" style="display:none">
                            <div class="media-progress-bar" id="media-progress-bar" style="width:0%"></div>
                        </div>
                    </div>
                </main>
            </div>
        </div>
    }
}

fn encode_key(key: &str) -> String {
    // 与 react 版一致:分段 URI 编码,斜杠保留
    key.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{:02X}", b),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[component]
fn MDir(node: MTree, current: Option<(String, String)>, on_play: Callback<(String, String)>) -> AnyView {
    if !has_media(&node) {
        return view! { <span style="display:none"></span> }.into_any();
    }
    let open = RwSignal::new(node.open);
    // 组件 prop 一律先装进信号:view! 的子闭包要求 Fn,捕获 owned 值会被 move 成 FnOnce
    let name = RwSignal::new(node.name);
    let dirs = RwSignal::new(node.dirs);
    let files = RwSignal::new(node.files);
    let current = RwSignal::new(current);
    let play = RwSignal::new(on_play);
    view! {
        <div class="oss-tdir">
            <div class="oss-trow" on:click=move |_| open.update(|o| *o = !*o)>
                <span class="oss-chev" class:oss-chev-open=move || open.get()>"▸"</span>
                <span class="oss-ticon">{move || if open.get() { "📂" } else { "📁" }}</span>
                <span class="oss-tname">{move || name.get()}</span>
            </div>
            <Show when=move || open.get()>
                <div class="oss-tkids">
                    <For
                        each=move || dirs.get()
                        key=|d: &MTree| d.prefix.clone()
                        children=move |d: MTree| view! {
                            <MDir node=d current=current.get() on_play=play.get()/>
                        }
                    />
                    <For
                        each=move || files.get()
                        key=|f: &OssEntry| f.key.clone()
                        children=move |f: OssEntry| {
                            let cur = current.get();
                            view! {
                                <MRow key=f.key.clone() name=f.name.clone() size=f.size
                                    active=cur.as_ref().map(|c| c.0 == f.key).unwrap_or(false)
                                    on_play=play.get()/>
                            }
                        }
                    />
                </div>
            </Show>
        </div>
    }
    .into_any()
}

#[component]
fn MRow(key: String, name: String, size: u64, active: bool, on_play: Callback<(String, String)>) -> impl IntoView {
    let icon = if is_audio(&name) { "🎵" } else { "🎬" };
    let k = key.clone();
    let n = name.clone();
    view! {
        <div class="oss-trow oss-tfile media-file-row" class:oss-active=active
            on:click=move |_| on_play.run((k.clone(), n.clone()))>
            <span class="oss-ticon">{icon}</span>
            <span class="oss-tname">{name.clone()}</span>
            <span class="media-size">{fmt_size(size)}</span>
        </div>
    }
}
