// 媒体库转码管线:ffmpeg.wasm 全量转码到浏览器安全格式
// (视频 → mp4/h264+aac+yuv420p+faststart,音频 → mp3 192k),
// 产物按对象 key 进 Cache API,二次播放免转码。
// 核心用 @ffmpeg/core 单线程版(coreURL/wasmURL 由 /public/ffmpeg 提供)。
import { FFmpeg } from '@ffmpeg/ffmpeg';
import { fetchFile, toBlobURL } from '@ffmpeg/util';

let engine: FFmpeg | null = null;
let loading: Promise<FFmpeg> | null = null;

async function getEngine(onLoad?: (pct: number | null) => void): Promise<FFmpeg> {
  if (engine?.loaded) return engine;
  loading ??= (async () => {
    onLoad?.(null); // 核心 ~31MB,首次下载无百分比
    const f = new FFmpeg();
    const base = `${(import.meta as any).env.BASE_URL || '/'}ffmpeg`;
    await f.load({
      coreURL: await toBlobURL(`${base}/ffmpeg-core.js`, 'text/javascript'),
      wasmURL: await toBlobURL(`${base}/ffmpeg-core.wasm`, 'application/wasm'),
    });
    engine = f;
    return f;
  })();
  return loading;
}

const CACHE = 'urlnote-media-v1';
const cacheKey = (key: string) => `${location.origin}/media-transcode/${encodeURIComponent(key)}`;

async function cached(key: string): Promise<Blob | null> {
  try {
    const hit = await (await caches.open(CACHE)).match(cacheKey(key));
    return hit ? await hit.blob() : null;
  } catch {
    return null;
  }
}

export interface Transcoded {
  blob: Blob;
  fromCache: boolean;
}

export function clearMediaCache(): Promise<boolean> {
  return caches.delete(CACHE).catch(() => false);
}

/// 全量转码(命中缓存直接返回)。onProgress:0-100 进度,null = 加载核心中。
export async function transcodeMedia(
  key: string,
  sourceUrl: string,
  onProgress: (pct: number | null) => void,
): Promise<Transcoded> {
  const hit = await cached(key);
  if (hit) return { blob: hit, fromCache: true };

  const f = await getEngine(onProgress);
  onProgress(0);
  const input = await fetchFile(sourceUrl);
  const inName = `in-${key.split('/').pop()}`;
  const isAudio = /\.mp3$/i.test(key);
  const outName = isAudio ? 'out.mp3' : 'out.mp4';
  await f.writeFile(inName, input);

  const onProg = ({ progress }: { progress: number }) => onProgress(Math.min(99, Math.round(progress * 100)));
  f.on('progress', onProg);
  const args = isAudio
    ? ['-i', inName, '-vn', '-c:a', 'libmp3lame', '-b:a', '192k', outName]
    : [
        '-i', inName,
        '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '23', '-pix_fmt', 'yuv420p',
        '-c:a', 'aac', '-b:a', '128k', '-movflags', 'faststart', outName,
      ];
  let code = 1;
  try {
    code = await f.exec(args);
  } finally {
    f.off('progress', onProg);
  }
  if (code !== 0) throw new Error(`转码失败 (ffmpeg exit ${code})`);

  const out = await f.readFile(outName);
  const bytes = typeof out === 'string' ? new TextEncoder().encode(out) : out;
  const blob = new Blob([bytes.buffer as ArrayBuffer], { type: isAudio ? 'audio/mpeg' : 'video/mp4' });
  try {
    await f.deleteFile(inName);
    await f.deleteFile(outName);
  } catch {
    /* 虚拟 FS 清理失败无妨 */
  }
  // 产物可能很大,缓存失败不阻塞播放
  try {
    await (await caches.open(CACHE)).put(cacheKey(key), new Response(blob));
  } catch {
    /* Cache API 不可用/超配额 */
  }
  onProgress(100);
  return { blob, fromCache: false };
}
