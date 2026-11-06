//! S3 操作 —— rust/frontend/src/oss.rs 的移植:GET/PUT/DELETE/LIST,
//! 浏览器内对 Sealos 对象存储(MinIO 兼容)端点做 SigV4 签名请求。
import { OSS_ENDPOINT, OSS_AK, OSS_SK, OSS_BUCKET, OSS_REGION } from './config';
import { awsUriEncode, canonicalQuery, signRequestWith } from './sigv4';
import type { OssEntry } from './tree';

const creds = { ak: OSS_AK, sk: OSS_SK, region: OSS_REGION, endpoint: OSS_ENDPOINT };
const utf8 = new TextEncoder();

/// Tauri 桌面端:webview(WKWebView)对对象存储的跨域直连会被拦
/// (TypeError: Load failed),改走 Rust 侧 reqwest(tauri-plugin-http);
/// Web 端保持浏览器原生 fetch。
let tauriFetch: typeof fetch | null | undefined;
async function nativeFetch(input: string, init?: RequestInit): Promise<Response> {
  if (tauriFetch === undefined && (window as unknown as { __TAURI__?: unknown }).__TAURI__) {
    try {
      tauriFetch = (await import('@tauri-apps/plugin-http')).fetch as unknown as typeof fetch;
    } catch {
      tauriFetch = null;
    }
  }
  if (tauriFetch) return tauriFetch(input, init);
  return fetch(input, init);
}

function signedHeaders(method: string, path: string, query: [string, string][], payload: Uint8Array) {
  const sig = signRequestWith(creds, method, path, query, payload, Date.now());
  return {
    'x-amz-date': sig.xAmzDate,
    'x-amz-content-sha256': sig.xAmzContentSha256,
    Authorization: sig.authorization,
  };
}

function objectUrl(key: string): { path: string; url: string } {
  const path = `/${OSS_BUCKET}/${awsUriEncode(key, false)}`;
  return { path, url: `https://${OSS_ENDPOINT}${path}` };
}

async function fetchWithSig(method: string, key: string, payload: Uint8Array<ArrayBuffer>): Promise<Response> {
  const { path, url } = objectUrl(key);
  try {
    return await nativeFetch(url, {
      method,
      headers: signedHeaders(method, path, [], payload),
      body: payload.length || method === 'PUT' ? payload : undefined,
    });
  } catch (e) {
    throw new Error(`网络错误: ${e}`);
  }
}

export async function getObject(key: string): Promise<string> {
  const r = await fetchWithSig('GET', key, new Uint8Array(0));
  if (r.status === 200) return r.text();
  if (r.status === 404) throw new Error('文件不存在');
  throw new Error(`下载失败 (HTTP ${r.status})`);
}

export async function getObjectBytes(key: string): Promise<Uint8Array<ArrayBuffer>> {
  const r = await fetchWithSig('GET', key, new Uint8Array(0));
  if (r.status === 200) return new Uint8Array(await r.arrayBuffer());
  if (r.status === 404) throw new Error('文件不存在');
  throw new Error(`下载失败 (HTTP ${r.status})`);
}

export async function deleteObject(key: string): Promise<void> {
  const r = await fetchWithSig('DELETE', key, new Uint8Array(0));
  if (r.status === 204 || r.status === 200) return;
  if (r.status === 404) throw new Error('文件不存在');
  throw new Error(`删除失败 (HTTP ${r.status})`);
}

/// key 是否已存在(覆盖确认用)。浏览器有 HEAD,直接签 HEAD,省掉 body 传输。
export async function objectExists(key: string): Promise<boolean> {
  const { path, url } = objectUrl(key);
  let r: Response;
  try {
    r = await nativeFetch(url, { method: 'HEAD', headers: signedHeaders('HEAD', path, [], new Uint8Array(0)) });
  } catch {
    return false;
  }
  return r.ok;
}

export async function putObject(key: string, content: string): Promise<void> {
  const r = await fetchWithSig('PUT', key, utf8.encode(content));
  if (!r.ok) throw new Error(`保存失败 (HTTP ${r.status})`);
}

/// 列出 prefix 下的全部对象(不分 delimiter,翻页跟到底)
export async function listAllEntriesUnder(prefix: string): Promise<OssEntry[]> {
  const out: OssEntry[] = [];
  let token: string | null = null;
  for (;;) {
    const query: [string, string][] = [
      ['list-type', '2'],
      ['max-keys', '1000'],
      ['prefix', prefix],
    ];
    if (token) query.push(['continuation-token', token]);
    const path = `/${OSS_BUCKET}`;
    let r: Response;
    const url = `https://${OSS_ENDPOINT}${path}?${canonicalQuery(query)}`;
    try {
      r = await nativeFetch(url, { headers: signedHeaders('GET', path, query, new Uint8Array(0)) });
    } catch (e) {
      throw new Error(`网络错误: ${e}`);
    }
    if (!r.ok) throw new Error(`列举失败 (HTTP ${r.status})`);
    const { entries, truncated, nextToken } = parseFullList(await r.text());
    out.push(...entries);
    if (truncated && nextToken) {
      token = nextToken;
    } else {
      break;
    }
  }
  return out;
}

/// 解析 ListObjectsV2 响应:entries、IsTruncated、NextContinuationToken。
/// DOMParser 命名空间无关(localName 匹配)。
export function parseFullList(xml: string): { entries: OssEntry[]; truncated: boolean; nextToken: string | null } {
  const doc = new DOMParser().parseFromString(xml, 'text/xml');
  if (doc.getElementsByTagName('parsererror').length > 0) {
    throw new Error('目录解析失败: XML 无效');
  }
  const entries: OssEntry[] = [];
  let truncated = false;
  let nextToken: string | null = null;
  const byLocal = (parent: Element | Document, name: string): Element[] =>
    Array.from(parent.getElementsByTagName('*')).filter((e) => e.localName === name);

  for (const contents of byLocal(doc, 'Contents')) {
    const key = byLocal(contents, 'Key')[0]?.textContent ?? null;
    if (key === null) continue;
    entries.push({
      name: key,
      key,
      size: Number(byLocal(contents, 'Size')[0]?.textContent ?? '0') || 0,
      modified: (byLocal(contents, 'LastModified')[0]?.textContent ?? '').slice(0, 10),
    });
  }
  const root = doc.documentElement;
  const truncEl = byLocal(root, 'IsTruncated')[0];
  if (truncEl) truncated = truncEl.textContent?.trim() === 'true';
  const tokenEl = byLocal(root, 'NextContinuationToken')[0];
  if (tokenEl) nextToken = tokenEl.textContent;
  return { entries, truncated, nextToken };
}
