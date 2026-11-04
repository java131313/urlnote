// 把 @ffmpeg/core 单线程核心拷进 public/ffmpeg(媒体库播放器的 coreURL/wasmURL
// 指向这里)。必须用单线程核心:站点未配 COOP/COEP 响应头,没有跨域隔离,
// 多线程核心的 SharedArrayBuffer 不可用。
import { cpSync, mkdirSync, rmSync } from 'node:fs';

const src = 'node_modules/@ffmpeg/core/dist/umd';
const dst = 'public/ffmpeg';
rmSync(dst, { recursive: true, force: true });
mkdirSync(dst, { recursive: true });
for (const f of ['ffmpeg-core.js', 'ffmpeg-core.wasm']) {
  cpSync(`${src}/${f}`, `${dst}/${f}`);
}
console.log('staged ffmpeg core →', dst);
