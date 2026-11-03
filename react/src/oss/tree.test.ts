//! rust/frontend/src/oss.rs tree_builds_nested_dirs_and_skips_markers 的移植
import { describe, expect, it } from 'vitest';
import { OssEntry, buildTree } from './tree';

describe('oss tree', () => {
  it('tree_builds_nested_dirs_and_skips_markers', () => {
    const entries: OssEntry[] = [
      { name: 'a/b.md', key: 'a/b.md', size: 1, modified: '' },
      { name: 'a/c/d.md', key: 'a/c/d.md', size: 1, modified: '' },
      { name: 'a/', key: 'a/', size: 0, modified: '' },
      { name: 'root.md', key: 'root.md', size: 1, modified: '' },
    ];
    const tree = buildTree(entries, undefined);
    expect(tree.files.length).toBe(1);
    expect(tree.dirs.length).toBe(1);
    const a = tree.dirs[0];
    expect(a.prefix).toBe('a/');
    expect(a.files.length).toBe(1);
    expect(a.dirs.length).toBe(1);
    expect(a.dirs[0].prefix).toBe('a/c/');
    expect(a.dirs[0].files[0].name).toBe('d.md');
  });

  it('刷新保持展开状态(按 prefix 继承)', () => {
    const entries: OssEntry[] = [{ name: 'a/b.md', key: 'a/b.md', size: 1, modified: '' }];
    const old = buildTree(entries, undefined);
    old.dirs[0].open = true;
    const rebuilt = buildTree(entries, old);
    expect(rebuilt.dirs[0].open).toBe(true);
  });
});
