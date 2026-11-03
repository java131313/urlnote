//! 目录树:S3 没有真目录 —— 由对象 key 按 / 切分,加上零字节 "name/" 标记对象。
//! 行为对齐 rust/frontend/src/oss.rs 的 TreeNode/build_tree。

export interface OssEntry {
  /// 相对所在前缀的文件名
  name: string;
  key: string;
  size: number;
  modified: string;
}

export interface TreeNode {
  /// 显示名(合成根为 "")
  name: string;
  /// 完整前缀,带尾部 "/"(根为 "")
  prefix: string;
  open: boolean;
  dirs: TreeNode[];
  files: OssEntry[];
}

export function emptyTree(): TreeNode {
  return { name: '', prefix: '', open: false, dirs: [], files: [] };
}

export function findNode(root: TreeNode, prefix: string): TreeNode | undefined {
  if (root.prefix === prefix) return root;
  for (const d of root.dirs) {
    const hit = findNode(d, prefix);
    if (hit) return hit;
  }
  return undefined;
}

/// 扁平列表 → 目录树。展开状态从旧树按 prefix 匹配继承,刷新时保持用户视图。
export function buildTree(entries: OssEntry[], old?: TreeNode): TreeNode {
  const root = emptyTree();
  for (const e of entries) {
    const parts = e.key.split('/');
    let node = root;
    let acc = '';
    const dirs = parts.slice(0, Math.max(0, parts.length - 1));
    for (const seg of dirs) {
      if (seg === '') continue;
      acc += seg + '/';
      if (!node.dirs.some((d) => d.prefix === acc)) {
        node.dirs.push({
          name: seg,
          prefix: acc,
          open: old ? (findNode(old, acc)?.open ?? false) : false,
          dirs: [],
          files: [],
        });
      }
      node = node.dirs.find((d) => d.prefix === acc)!;
    }
    const last = parts[parts.length - 1];
    if (last === '') {
      // 文件夹标记对象 —— 目录本身已在上面建好
    } else {
      node.files.push({ name: last, key: e.key, size: e.size, modified: e.modified });
    }
  }
  const sortRec = (n: TreeNode) => {
    n.dirs.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    n.files.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    n.dirs.forEach(sortRec);
  };
  sortRec(root);
  return root;
}
