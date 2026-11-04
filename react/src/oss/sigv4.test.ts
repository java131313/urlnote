//! 与 rust/frontend/src/oss.rs 的 #[cfg(test)] 同一组测试向量
//! (签名向量由独立 Python 参考实现生成,两版签名器必须逐字节一致)。
import { describe, expect, it } from 'vitest';
import { awsUriEncode, fmtTimes, hmacSha256, sha256Hex, signRequestWith, toHex } from './sigv4';
import { utf8From } from './testUtil';

describe('sigv4', () => {
  it('sha256 已知向量', () => {
    expect(sha256Hex(new Uint8Array(0))).toBe(
      'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
    );
    expect(sha256Hex(utf8From('abc'))).toBe(
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
    // 跨块(>64 字节)+ 补位边界
    expect(sha256Hex(utf8From('a'.repeat(55)))).toBe(
      '9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318',
    );
  });

  it('hmac-sha256 RFC 4231 测试用例 1', () => {
    // key = 0x0b × 20, msg = "Hi There"
    expect(toHex(hmacSha256(new Uint8Array(20).fill(0x0b), utf8From('Hi There')))).toBe(
      'b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7',
    );
  });

  it('fmtTimes → UTC 字符串', () => {
    expect(fmtTimes(1767323045000)).toEqual(['20260102T030405Z', '20260102']);
    expect(fmtTimes(0)).toEqual(['19700101T000000Z', '19700101']);
  });

  it('signer_matches_python_reference', () => {
    const sig = signRequestWith(
      { ak: 'TESTACCESSKEY', sk: 'fake-secret-key-for-unit-tests', region: 'us-east-1', endpoint: 'objectstorageapi.bja.sealos.run' },
      'GET',
      '/test-bucket',
      [],
      new Uint8Array(0),
      1767323045000,
    );
    expect(sig.xAmzDate).toBe('20260102T030405Z');
    expect(sig.xAmzContentSha256).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    expect(sig.authorization).toBe(
      'AWS4-HMAC-SHA256 Credential=TESTACCESSKEY/20260102/us-east-1/s3/aws4_request, ' +
        'SignedHeaders=host;x-amz-content-sha256;x-amz-date, ' +
        'Signature=61df9983846c3a678d1499dd8b7057c2c9ead369bf3e266b292146bacae7314f',
    );
  });

  it('uri_encode_keeps_unreserved_and_slash', () => {
    expect(awsUriEncode('go语言视频/x y.md', false)).toBe('go%E8%AF%AD%E8%A8%80%E8%A7%86%E9%A2%91/x%20y.md');
    expect(awsUriEncode('a/b', true)).toBe('a%2Fb');
    expect(awsUriEncode('a-b_c.d~e', true)).toBe('a-b_c.d~e');
  });
});
