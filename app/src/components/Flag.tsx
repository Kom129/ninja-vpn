// Флаг страны картинкой (SVG из пакета country-flag-icons, лежит внутри приложения).

import type { ComponentType, SVGProps } from 'react';
import * as Flags from 'country-flag-icons/react/3x2';

const table = Flags as unknown as Record<string, ComponentType<SVGProps<SVGSVGElement>>>;

export function Flag({ code, className }: { code: string | null; className?: string }) {
  const Svg = code ? table[code.toUpperCase()] : undefined;
  if (!Svg) return null;
  return <Svg className={className} preserveAspectRatio="xMidYMid slice" aria-hidden="true" />;
}
