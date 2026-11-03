//! SigV4 请求签名 —— rust/frontend/src/oss.rs 的逐行移植。
//! 纯 TS 实现 SHA-256/HMAC(签名是同步热路径,不走 WebCrypto),
//! 与 Rust 版共用同一组测试向量(tests/ 下,来自独立 Python 参考实现)。

// ---- SHA-256 (FIPS 180-4) ----

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const rotr = (x: number, n: number): number => (x >>> n) | (x << (32 - n));

export function sha256(data: Uint8Array): Uint8Array {
  const h = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  // 补位到 64 的倍数,末尾 64 位大端位长(2×32bit)
  const bitLenHi = Math.floor(data.length / 0x20000000);
  const bitLenLo = (data.length << 3) >>> 0;
  const paddedLen = ((((data.length + 8) >> 6) + 1) << 6);
  const buf = new Uint8Array(paddedLen);
  buf.set(data);
  buf[data.length] = 0x80;
  const dv = new DataView(buf.buffer);
  dv.setUint32(paddedLen - 8, bitLenHi);
  dv.setUint32(paddedLen - 4, bitLenLo);

  const w = new Uint32Array(64);
  for (let off = 0; off < paddedLen; off += 64) {
    for (let t = 0; t < 16; t++) w[t] = dv.getUint32(off + t * 4);
    for (let t = 16; t < 64; t++) {
      const x = w[t - 15];
      const y = w[t - 2];
      const s0 = rotr(x, 7) ^ rotr(x, 18) ^ (x >>> 3);
      const s1 = rotr(y, 17) ^ rotr(y, 19) ^ (y >>> 10);
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) >>> 0;
    }
    let a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
    for (let t = 0; t < 64; t++) {
      const s1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + s1 + ch + K[t] + w[t]) >>> 0;
      const s0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (s0 + maj) >>> 0;
      hh = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    h[0] = (h[0] + a) >>> 0; h[1] = (h[1] + b) >>> 0; h[2] = (h[2] + c) >>> 0; h[3] = (h[3] + d) >>> 0;
    h[4] = (h[4] + e) >>> 0; h[5] = (h[5] + f) >>> 0; h[6] = (h[6] + g) >>> 0; h[7] = (h[7] + hh) >>> 0;
  }
  const out = new Uint8Array(32);
  const odv = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) odv.setUint32(i * 4, h[i]);
  return out;
}

const SHA256_BLOCK = 64;

export function hmacSha256(key: Uint8Array, msg: Uint8Array): Uint8Array {
  let k = key;
  if (k.length > SHA256_BLOCK) k = sha256(k);
  const inner = new Uint8Array(SHA256_BLOCK + msg.length);
  const outer = new Uint8Array(SHA256_BLOCK + 32);
  for (let i = 0; i < SHA256_BLOCK; i++) {
    const b = i < k.length ? k[i] : 0;
    inner[i] = b ^ 0x36;
    outer[i] = b ^ 0x5c;
  }
  inner.set(msg, SHA256_BLOCK);
  outer.set(sha256(inner), SHA256_BLOCK);
  return sha256(outer);
}

export function toHex(b: Uint8Array): string {
  let s = '';
  for (const x of b) s += x.toString(16).padStart(2, '0');
  return s;
}

export function sha256Hex(b: Uint8Array): string {
  return toHex(sha256(b));
}

// ---- SigV4 ----

const utf8 = new TextEncoder();

/// URI-encode per RFC 3986 (S3 variant):保留非保留字符,其余按 UTF-8 字节 %XX。
/// encode_slash=true 时 / 也编码(查询串),false 保留(对象 key)。
export function awsUriEncode(s: string, encodeSlash: boolean): string {
  const bytes = utf8.encode(s);
  let out = '';
  for (const b of bytes) {
    if (
      (b >= 0x41 && b <= 0x5a) || (b >= 0x61 && b <= 0x7a) || (b >= 0x30 && b <= 0x39) ||
      b === 0x2d || b === 0x5f || b === 0x2e || b === 0x7e
    ) {
      out += String.fromCharCode(b);
    } else if (b === 0x2f && !encodeSlash) {
      out += '/';
    } else {
      out += '%' + b.toString(16).toUpperCase().padStart(2, '0');
    }
  }
  return out;
}

/// (amz_date, date_stamp) —— epoch 毫秒 → UTC,Howard Hinnant civil_from_days
export function fmtTimes(ms: number): [string, string] {
  const secs = Math.floor(ms / 1000);
  const days = Math.floor(secs / 86400);
  const rem = secs % 86400;
  const hh = Math.floor(rem / 3600);
  const mi = Math.floor((rem % 3600) / 60);
  const ss = rem % 60;
  const z = days + 719468;
  const era = Math.floor(z / 146097);
  const doe = z - era * 146097;
  const yoe = Math.floor((doe - Math.floor(doe / 1460) + Math.floor(doe / 36524) - Math.floor(doe / 146096)) / 365);
  let y = yoe + era * 400;
  const doy = doe - (365 * yoe + Math.floor(yoe / 4) - Math.floor(yoe / 100));
  const mp = Math.floor((5 * doy + 2) / 153);
  const d = doy - Math.floor((153 * mp + 2) / 5) + 1;
  const m = mp < 10 ? mp + 3 : mp - 9;
  if (m <= 2) y += 1;
  const p = (n: number) => String(n).padStart(2, '0');
  return [`${y}${p(m)}${p(d)}T${p(hh)}${p(mi)}${p(ss)}Z`, `${y}${p(m)}${p(d)}`];
}

export interface SigHeaders {
  authorization: string;
  xAmzDate: string;
  xAmzContentSha256: string;
}

export interface OssCreds {
  ak: string;
  sk: string;
  region: string;
  endpoint: string;
}

/// 规范查询串:编码 + 按 (key, value) 字典序 —— 签名与请求 URL 共用同一串
export function canonicalQuery(query: [string, string][]): string {
  return query
    .map(([k, v]) => [awsUriEncode(k, true), awsUriEncode(v, true)] as [string, string])
    .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0))
    .map(([k, v]) => `${k}=${v}`)
    .join('&');
}

/// 签名请求。path 必须已编码(斜杠保留);query 在此编码。
/// 只签 SigV4 必需的三个头,浏览器附加的头(Accept、Content-Type…)
/// 不进签名(SigV4 允许)。
export function signRequestWith(
  creds: OssCreds,
  method: string,
  path: string,
  query: [string, string][],
  payload: Uint8Array,
  nowMs: number,
): SigHeaders {
  const [amz, dateStamp] = fmtTimes(nowMs);
  const payloadHash = sha256Hex(payload);
  const queryStr = canonicalQuery(query);

  const canonical =
    `${method}\n${path}\n${queryStr}\n` +
    `host:${creds.endpoint}\nx-amz-content-sha256:${payloadHash}\nx-amz-date:${amz}\n\n` +
    `host;x-amz-content-sha256;x-amz-date\n${payloadHash}`;
  const scope = `${dateStamp}/${creds.region}/s3/aws4_request`;
  const stringToSign = `AWS4-HMAC-SHA256\n${amz}\n${scope}\n${sha256Hex(utf8.encode(canonical))}`;

  let k = hmacSha256(utf8.encode(`AWS4${creds.sk}`), utf8.encode(dateStamp));
  k = hmacSha256(k, utf8.encode(creds.region));
  k = hmacSha256(k, utf8.encode('s3'));
  k = hmacSha256(k, utf8.encode('aws4_request'));
  const sig = toHex(hmacSha256(k, utf8.encode(stringToSign)));

  return {
    authorization:
      `AWS4-HMAC-SHA256 Credential=${creds.ak}/${scope}, ` +
      `SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=${sig}`,
    xAmzDate: amz,
    xAmzContentSha256: payloadHash,
  };
}
