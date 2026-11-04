// 改 zoom(字号)前后保持光标视口位置。zoom 直接作用在滚动容器上,布局
// 重排会让光标视觉上跳动 relY·Δz(relY 为光标距视口顶的未缩放距离);
// scrollTop 是不经 zoom 的 CSS px,视口差值除以当前 zoom 补回即可还原。
// 浏览器实测:连续 4 步放大(1.125→1.5)光标视口位置误差 0。
// 选区不是折叠光标(光标不在编辑器里)时跳过。
export function keepCaretPos(apply: () => void, afterLayout?: () => void) {
  const sel = window.getSelection();
  let range: Range | null = null;
  let top = 0;
  if (sel && sel.rangeCount > 0 && sel.isCollapsed) {
    const r = sel.getRangeAt(0).cloneRange();
    const rect = r.getBoundingClientRect();
    if (rect && !(rect.left === 0 && rect.top === 0 && rect.right === 0 && rect.bottom === 0)) {
      range = r;
      top = rect.top;
    }
  }
  apply();
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      afterLayout?.();
      if (!range) return;
      const dy = range!.getBoundingClientRect().top - top;
      if (Math.abs(dy) < 1) return;
      let el: Element | null =
        range!.startContainer.nodeType === Node.ELEMENT_NODE
          ? (range!.startContainer as Element)
          : range!.startContainer.parentElement;
      while (el && el !== document.documentElement) {
        const oy = getComputedStyle(el).overflowY;
        if ((oy === 'auto' || oy === 'scroll') && el.scrollHeight > el.clientHeight + 2) {
          // scrollTop 不经 zoom,视口差值要除以当前 zoom 补回(浏览器实测验证)
          (el as HTMLElement).scrollTop += dy / zoomOf(el);
          return;
        }
        el = el.parentElement;
      }
      window.scrollBy(0, dy);
    });
  });
}

/// 元素当前生效的 zoom:优先 computed zoom(部分浏览器返回 "normal"),
/// 退回 rectHeight/offsetHeight 几何测量
function zoomOf(el: Element): number {
  const z = parseFloat(getComputedStyle(el).zoom);
  if (z > 0) return z;
  const rectH = el.getBoundingClientRect().height;
  const offH = (el as HTMLElement).offsetHeight;
  return rectH > 0 && offH > 0 ? rectH / offH : 1;
}

/// Vditor 给每个块级 pre 内联了居中 padding(如 padding: 10px 231px),
/// zoom 会把它一起放大,导致字体变大时两边留白变宽。按 1/zoom 反写内联
/// padding,保持留白视觉宽度不变。基准与上次反写值记在 data 属性里;
/// 当前值与上次反写不符 = Vditor 重算过(窗口 resize),重新捕获基准。
export function applyZoomPadding(root: ParentNode | null, zoom: number) {
  if (!root || !(zoom > 0)) return;
  root
    .querySelectorAll<HTMLElement>('.vditor-wysiwyg pre.vditor-reset, .vditor-preview > .vditor-reset')
    .forEach((el) => {
      const cs = getComputedStyle(el);
      const curL = parseFloat(cs.paddingLeft) || 0;
      const curR = parseFloat(cs.paddingRight) || 0;
      if (curL <= 0 && curR <= 0) return;
      let baseL = curL;
      let baseR = curR;
      const w = el.dataset.zpadW?.split(',').map(Number);
      if (w && w.length === 2 && Math.abs(curL - w[0]) < 0.6 && Math.abs(curR - w[1]) < 0.6) {
        const b = el.dataset.zpadB?.split(',').map(Number);
        if (b && b.length === 2) {
          baseL = b[0];
          baseR = b[1];
        }
      }
      const nl = baseL / zoom;
      const nr = baseR / zoom;
      el.style.paddingLeft = `${nl}px`;
      el.style.paddingRight = `${nr}px`;
      el.dataset.zpadB = `${baseL},${baseR}`;
      el.dataset.zpadW = `${nl},${nr}`;
    });
}
