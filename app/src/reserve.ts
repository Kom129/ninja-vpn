// Резерв: какие серверы можно пробовать, если выбранный не ответил, и в каком порядке.
//
// Правило из исследования (docs/research/REPORT.md, §9): переключаться только внутри
// того, что разрешил человек, — не подсовывать молча другую страну или чужой сервис.
// Поэтому по умолчанию резерв — только та же группа того же источника.

import type { ProbeResult, Server, Source } from './api';
import { COUNTRIES, parseServerName } from './names';

export type ReservePolicy = 'off' | 'group' | 'source' | 'all';

/** Выбранный сервер + не больше трёх резервных (так же ограничено в мосте окна). */
export const MAX_ATTEMPTS = 4;

/** Варианты резерва по порядку; подписи — в словарях (reserve.off, reserve.off.hint…). */
export const RESERVE_OPTIONS: ReservePolicy[] = ['off', 'group', 'source', 'all'];

/** Группа сервера внутри источника — так же, как в панели профилей. */
export function groupOf(source: Source, server: Server): string {
  return source.kind === 'key' ? '' : (parseServerName(server.name).group ?? COUNTRIES);
}

/** Порядок: проверенные — по задержке, потом непроверенные, в конце не ответившие. */
export function speedRank(probes: Record<string, ProbeResult>, key: string): number {
  const p = probes[key];
  if (!p) return 1e6;
  return p.ms ?? 2e6;
}

/** Ключи серверов для подключения: выбранный первым, дальше разрешённые резервные по скорости. */
export function connectPlan(
  selectedKey: string,
  sources: Source[],
  probes: Record<string, ProbeResult>,
  policy: ReservePolicy,
): string[] {
  const home = sources.find((s) => s.servers.some((x) => x.key === selectedKey));
  const selected = home?.servers.find((x) => x.key === selectedKey);
  if (!home || !selected || policy === 'off') return [selectedKey];
  const group = groupOf(home, selected);
  const pool = sources
    .filter((s) => policy === 'all' || s.id === home.id)
    .flatMap((s) => s.servers.filter((x) => !x.problem && x.key !== selectedKey).map((x) => ({ s, x })))
    .filter(({ s, x }) => policy !== 'group' || groupOf(s, x) === group)
    .sort((a, b) => speedRank(probes, a.x.key) - speedRank(probes, b.x.key))
    .map(({ x }) => x.key);
  return [selectedKey, ...pool].slice(0, MAX_ATTEMPTS);
}
