//! 媒体库(#/media):左侧 S3 文件树(只显示 mp3/mp4 与文件夹),
//! 右侧 ffmpeg.wasm 全量转码后的 <video>/<audio> 播放器。
//! 文件流经后端 /api/media/stream/<key>(会话鉴权 + 服务端 SigV4 代理);
//! 转码产物按 key 缓存(Cache API),二次播放即时。
import { useCallback, useEffect, useRef, useState } from 'react';
import { OSS_ENABLED } from '../oss/config';
import { listAllEntriesUnder } from '../oss/s3';
import { TreeNode, buildTree, emptyTree } from '../oss/tree';
import { clearMediaCache, transcodeMedia } from './mediaTranscode';

const isMedia = (name: string) => /\.(mp3|mp4)$/i.test(name);
const isAudioName = (name: string) => /\.mp3$/i.test(name);
const mediaIcon = (name: string) => (isAudioName(name) ? '🎵' : '🎬');

function fmtSize(n?: number): string {
  if (!n || n <= 0) return '';
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

type Phase = 'idle' | 'engine' | 'working' | 'ready' | 'error';

export function MediaRoute() {
  if (!OSS_ENABLED) {
    return (
      <div className="media-page">
        <div className="oss-top">
          <a className="oss-home" href="#/">
            📝 便签
          </a>
          <span className="oss-brand">🎬 媒体库</span>
        </div>
        <div className="oss-empty">对象存储未配置:需要 VITE_OSS_AK / VITE_OSS_BUCKET 构建参数</div>
      </div>
    );
  }
  return <MediaPage />;
}

function MediaPage() {
  const [tree, setTree] = useState<TreeNode>(emptyTree);
  const [status, setStatus] = useState('');
  const [current, setCurrent] = useState<{ key: string; name: string; size?: number } | null>(null);
  const [phase, setPhase] = useState<Phase>('idle');
  const [pct, setPct] = useState(0);
  const [playUrl, setPlayUrl] = useState<string | null>(null);
  const [, setVer] = useState(0);
  const bump = useCallback(() => setVer((v) => v + 1), []);
  const playUrlRef = useRef<string | null>(null);

  const refresh = useCallback(() => {
    setStatus('加载目录…');
    listAllEntriesUnder('')
      .then((entries) => {
        setTree((t) => buildTree(entries, t));
        setStatus('');
      })
      .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));
  }, []);
  useEffect(refresh, [refresh]);

  const revoke = () => {
    if (playUrlRef.current) URL.revokeObjectURL(playUrlRef.current);
    playUrlRef.current = null;
  };
  useEffect(() => revoke, []);

  const play = (key: string, name: string, size?: number) => {
    revoke();
    setPlayUrl(null);
    setCurrent({ key, name, size });
    setPhase('engine');
    setPct(0);
    setStatus('');
    const streamUrl = `/api/media/stream/${key.split('/').map(encodeURIComponent).join('/')}`;
    transcodeMedia(key, streamUrl, (p) => {
      if (p === null) setPhase('engine');
      else {
        setPhase('working');
        setPct(p);
      }
    })
      .then((r) => {
        const u = URL.createObjectURL(r.blob);
        playUrlRef.current = u;
        setPlayUrl(u);
        setPhase('ready');
        setStatus(r.fromCache ? '缓存命中,直接播放' : '转码完成');
      })
      .catch((e) => {
        setPhase('error');
        setStatus(e instanceof Error ? e.message : String(e));
      });
  };

  return (
    <div className="media-page">
      <div className="oss-top">
        <a className="oss-home" href="#/" title="返回便签页面">
          📝 便签
        </a>
        <span className="oss-brand">🎬 媒体库</span>
        <span className="oss-spacer" />
        <button type="button" className="tool" onClick={refresh} title="重新加载目录">
          🔄
        </button>
        <button
          type="button"
          className="tool"
          title="清空转码缓存(下次播放重新转码)"
          onClick={() => clearMediaCache().then((ok) => setStatus(ok ? '已清空转码缓存' : '缓存不可用'))}
        >
          🧹 清缓存
        </button>
        <span className="oss-status">{status}</span>
      </div>
      <div className="media-body">
        <aside className="media-side">
          <div className="oss-tree">
            {tree.dirs.length === 0 && tree.files.length === 0 && (
              <div className="oss-hint">(空桶,把 mp3/mp4 放进对象存储后点上方刷新)</div>
            )}
            {tree.dirs.map((d) => (
              <MediaDir key={d.prefix} node={d} currentKey={current?.key} onToggle={bump} onPlay={play} depth={0} />
            ))}
            {tree.files
              .filter((f) => isMedia(f.name))
              .map((f) => (
                <MediaFileRow key={f.key} name={f.name} size={f.size} active={current?.key === f.key} onPlay={() => play(f.key, f.name, f.size)} depth={0} />
              ))}
          </div>
        </aside>
        <main className="media-main">
          {current ? (
            <>
              <div className="media-filebar">
                <span className="media-title">
                  {current.name}
                  {current.size ? <span className="media-size">{fmtSize(current.size)}</span> : null}
                </span>
              </div>
              <div className="media-stage">
                {playUrl ? (
                  isAudioName(current.name) ? (
                    <audio controls autoPlay src={playUrl} className="media-audio" />
                  ) : (
                    <video controls autoPlay src={playUrl} className="media-video" />
                  )
                ) : (
                  <div className="media-hint">
                    {phase === 'engine' && '首次使用:正在加载 ffmpeg 核心(约 31MB,之后缓存)…'}
                    {phase === 'working' && `全量转码中 ${pct}%…`}
                    {phase === 'error' && `⚠️ ${status}`}
                  </div>
                )}
                {(phase === 'engine' || phase === 'working') && (
                  <div className="media-progress">
                    <div className="media-progress-bar" style={{ width: `${phase === 'engine' ? 8 : pct}%` }} />
                  </div>
                )}
              </div>
            </>
          ) : (
            <div className="media-hint">从左侧选择一个 mp3 / mp4 开始播放(首次播放需全量转码)</div>
          )}
        </main>
      </div>
    </div>
  );
}

function MediaDir(props: {
  node: TreeNode;
  currentKey?: string;
  depth: number;
  onToggle: () => void;
  onPlay: (key: string, name: string, size?: number) => void;
}) {
  const { node } = props;
  const mediaFiles = node.files.filter((f) => isMedia(f.name));
  if (mediaFiles.length === 0 && node.dirs.length === 0) return null; // 没有媒体的空目录不显示
  return (
    <div className="oss-tdir">
      <div className="oss-trow" onClick={() => props.onToggle()}>
        <span className={'oss-chev' + (node.open ? ' oss-chev-open' : '')}>▸</span>
        <span className="oss-ticon">{node.open ? '📂' : '📁'}</span>
        <span className="oss-tname">{node.name}</span>
      </div>
      {node.open && (
        <div className="oss-tkids">
          {node.dirs.map((d) => (
            <MediaDir key={d.prefix} {...props} node={d} depth={props.depth + 1} />
          ))}
          {mediaFiles.map((f) => (
            <MediaFileRow
              key={f.key}
              name={f.name}
              size={f.size}
              active={props.currentKey === f.key}
              onPlay={() => props.onPlay(f.key, f.name, f.size)}
              depth={props.depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function MediaFileRow(props: {
  name: string;
  size?: number;
  depth: number;
  active: boolean;
  onPlay: () => void;
}) {
  return (
    <div className={'oss-trow oss-tfile media-file-row' + (props.active ? ' oss-active' : '')} onClick={props.onPlay}>
      <span className="oss-ticon">{mediaIcon(props.name)}</span>
      <span className="oss-tname">{props.name}</span>
      {props.size ? <span className="media-size">{fmtSize(props.size)}</span> : null}
    </div>
  );
}
