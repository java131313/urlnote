// 把 vditor 运行时资源拷进 public/,供 Vditor 的 cdn 选项使用(/vditor)
import { cpSync, mkdirSync, rmSync } from 'node:fs';

const src = 'node_modules/vditor/dist';
const dst = 'public/vditor/dist';
rmSync('public/vditor', { recursive: true, force: true });
mkdirSync('public/vditor', { recursive: true });
cpSync(src, dst, { recursive: true });
console.log('staged vditor ->', dst);
