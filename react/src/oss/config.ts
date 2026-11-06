// 对象存储配置:构建期注入(Vite 静态替换 import.meta.env.VITE_*),
// 与 Rust 前端的编译期 option_env! 同机制。密钥会打进前端包,
// 与 Rust 版同样的安全模型(个人使用,见 Rust oss.rs 头部注释)。
// endpoint 兼容带/不带 scheme 两种传法(s3.ts 拼接时会自己补 https://)。
export const OSS_ENDPOINT: string = ((import.meta as any).env.VITE_OSS_ENDPOINT || 'objectstorageapi.bja.sealos.run')
  .replace(/^https?:\/\//, '');
export const OSS_AK: string = (import.meta as any).env.VITE_OSS_AK || '';
export const OSS_SK: string = (import.meta as any).env.VITE_OSS_SK || '';
export const OSS_BUCKET: string = (import.meta as any).env.VITE_OSS_BUCKET || '';
export const OSS_REGION: string = (import.meta as any).env.VITE_OSS_REGION || 'us-east-1';

/// 仅在 AK/BUCKET 都配置时才真正发请求;否则页面显示"未配置"提示
export const OSS_ENABLED: boolean = OSS_AK !== '' && OSS_BUCKET !== '';
