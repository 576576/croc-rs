import type { ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';

/// 可展开的一块：标题两头对齐，右侧一枚小箭头，展开时转半圈。
/// 箭头不能省——`details > summary` 的 `list-style: none` 把浏览器自带的三角去掉了，
/// 没有这枚箭头就看不出这一行能点开。
export function Disclosure({ label, children }: { label: string; children: ReactNode }) {
  return (
    <details className="disclosure">
      <summary className="field-label disclosure-summary">
        <span>{label}</span>
        <ChevronDown className="disclosure-arrow" aria-hidden="true" />
      </summary>
      {children}
    </details>
  );
}
