// 改 zoom(字号)前后保持光标视口位置。zoom 直接作用在滚动容器上,布局
// 重排会让光标视觉上跳动 relY·Δz(relY 为光标距视口顶的未缩放距离);
// scrollTop 是不经 zoom 的 CSS px,视口差值除以当前 zoom 补回即可还原。
// 浏览器实测:连续 4 步放大(1.125→1.5)光标视口位置误差 0。
// 选区不是折叠光标(光标不在编辑器里)时跳过。
export function keepCaretPos(apply: () => void) {
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
  if (!range) return;
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
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
