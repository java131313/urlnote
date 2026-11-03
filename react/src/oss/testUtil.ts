// TextEncoder 在浏览器与 Node ≥ 18 均为全局,测试直接用之。
export const utf8From = (s: string): Uint8Array => new TextEncoder().encode(s);
