// Выдвижная панель «Профили»: источники (подписки и ключи) и их серверы.
// Источник можно добавить, обновить, переименовать и удалить; серверы сгруппированы как у сервиса.

import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { ChevronDown, ChevronsDownUp, ChevronsUpDown, EllipsisVertical, Gauge, KeyRound, Link2, Plus, RefreshCw, X } from 'lucide-react';
import type { ProbeResult, Server, Source } from '../api';
import { COUNTRIES, parseServerName, transportOf } from '../names';
import { useI18n, type I18n } from '../i18n';
import { Flag } from './Flag';

interface Props {
  sources: Source[] | null;
  error: string | null;
  selectedKey: string | null;
  probes: Record<string, ProbeResult>;
  /** Нельзя удалять источники (подключено или идёт подключение). */
  locked: boolean;
  /** Нельзя выбирать сервер: идёт подключение или отключение. Во время работы — можно: переключимся на лету. */
  selectLocked: boolean;
  onSelect(key: string): void;
  onAdd(name: string | null, link: string): Promise<void>;
  onRefresh(id: string): Promise<void>;
  onRename(id: string, name: string): Promise<void>;
  onRemove(id: string): Promise<void>;
  onProbe(id: string): Promise<void>;
  onClose(): void;
}

const gib = (i18n: I18n, bytes: number) => i18n.t('panel.gb', { n: bytes / 1024 ** 3 });

/** «2027-03-17» → дата в записи языка: 17.03.2027, 3/17/2027, 2027/3/17… */
function date(i18n: I18n, iso: string) {
  const [y, m, d] = iso.split('-').map(Number);
  if (!y || !m || !d) return iso;
  return new Intl.DateTimeFormat(i18n.info.locale).format(new Date(y, m - 1, d));
}

const COLLAPSED_KEY = 'ninja.collapsed';

/** Какие источники свёрнуты — запоминаем между запусками (если хранилище браузера доступно). */
function useCollapsed(): [Set<string>, (next: Set<string>) => void] {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => {
    try {
      return new Set(JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? '[]') as string[]);
    } catch {
      return new Set();
    }
  });
  const save = (next: Set<string>) => {
    setCollapsed(next);
    try {
      localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next]));
    } catch {
      /* не запомним — не страшно */
    }
  };
  return [collapsed, save];
}

/** Порядок внутри группы: проверенные — по задержке, потом непроверенные, в конце не ответившие. */
function bySpeed(probes: Record<string, ProbeResult>) {
  const rank = (s: Server) => {
    const p = probes[s.key];
    if (!p) return 1e6;
    return p.ms ?? 2e6;
  };
  return (a: Server, b: Server) => rank(a) - rank(b);
}

/** «обновлено 5 минут назад» — «5 минут назад» оформляет сам язык (Intl.RelativeTimeFormat). */
function ago(i18n: I18n, unix: number) {
  const minutes = Math.max(0, Math.round((Date.now() / 1000 - unix) / 60));
  if (minutes < 1) return i18n.t('panel.updatedNow');
  const rtf = new Intl.RelativeTimeFormat(i18n.info.locale, { numeric: 'always' });
  const hours = Math.round(minutes / 60);
  const when = minutes < 60 ? rtf.format(-minutes, 'minute') : hours < 24 ? rtf.format(-hours, 'hour') : rtf.format(-Math.round(hours / 24), 'day');
  return i18n.t('panel.updated', { ago: when });
}

export function ServerPanel(props: Props) {
  const { t } = useI18n();
  const { sources, error, onClose } = props;
  const [adding, setAdding] = useState(false);
  const [collapsed, setCollapsed] = useCollapsed();
  const [probingAll, setProbingAll] = useState(false);
  const probeAll = async () => {
    setProbingAll(true);
    try {
      for (const s of sources ?? []) await props.onProbe(s.id).catch(() => {});
    } finally {
      setProbingAll(false);
    }
  };
  const ids = sources?.map((s) => s.id) ?? [];
  const allCollapsed = ids.length > 0 && ids.every((id) => collapsed.has(id));
  const toggle = (id: string) => {
    const next = new Set(collapsed);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setCollapsed(next);
  };

  return (
    <aside className="panel" aria-label={t('panel.title')}>
      <div className="panel-top" data-tauri-drag-region />
      <div className="panel-head">
        <h2>{t('panel.title')}</h2>
        {ids.length > 0 && (
          <button
            className="icon-btn"
            onClick={probeAll}
            disabled={probingAll}
            aria-label={t('panel.probeAll')}
            title={t('panel.probeAll')}
          >
            <Gauge size={19} strokeWidth={1.6} className={probingAll ? 'pulse' : ''} />
          </button>
        )}
        {ids.length > 1 && (
          <button
            className="icon-btn"
            onClick={() => setCollapsed(allCollapsed ? new Set() : new Set(ids))}
            aria-label={allCollapsed ? t('panel.expandAllAria') : t('panel.collapseAllAria')}
            title={allCollapsed ? t('panel.expandAll') : t('panel.collapseAll')}
          >
            {allCollapsed ? <ChevronsUpDown size={19} strokeWidth={1.6} /> : <ChevronsDownUp size={19} strokeWidth={1.6} />}
          </button>
        )}
        <button className="icon-btn" onClick={onClose} aria-label={t('panel.hide')}>
          <X size={20} strokeWidth={1.6} />
        </button>
      </div>

      <div className="panel-list">
        {error && <p className="panel-note bad">{error}</p>}
        {sources && sources.length === 0 && !adding && (
          <p className="panel-note">{t('panel.empty')}</p>
        )}
        {sources?.map((s) => (
          <SourceSection key={s.id} source={s} collapsed={collapsed.has(s.id)} onToggle={() => toggle(s.id)} {...props} />
        ))}
      </div>

      <div className="panel-foot">
        {adding ? (
          <AddForm onAdd={props.onAdd} onDone={() => setAdding(false)} />
        ) : (
          <button className="add" onClick={() => setAdding(true)}>
            <Plus size={18} strokeWidth={1.6} /> {t('panel.add')}
          </button>
        )}
      </div>
    </aside>
  );
}

function SourceSection({
  source,
  collapsed,
  onToggle,
  selectedKey,
  probes,
  locked,
  selectLocked,
  onSelect,
  onProbe,
  onRefresh,
  onRename,
  onRemove,
}: Props & { source: Source; collapsed: boolean; onToggle(): void }) {
  const i18n = useI18n();
  const { t } = i18n;
  const [menu, setMenu] = useState(false);
  const [mode, setMode] = useState<'view' | 'rename' | 'remove'>('view');
  // Переименование: старое имя сразу выделено — новое печатается поверх.
  const renameInput = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (mode === 'rename') renameInput.current?.select();
  }, [mode]);
  const [name, setName] = useState(source.name);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  const groups = useMemo(() => {
    const map = new Map<string, { icon: string | null; servers: Server[] }>();
    for (const s of source.servers) {
      const n = parseServerName(s.name);
      const group = source.kind === 'key' ? '' : (n.group ?? COUNTRIES);
      if (!map.has(group)) map.set(group, { icon: n.groupIcon, servers: [] });
      map.get(group)!.servers.push(s);
    }
    const order = bySpeed(probes);
    for (const g of map.values()) g.servers.sort(order);
    return [...map.entries()];
  }, [source, probes]);

  const selected = source.servers.find((s) => s.key === selectedKey) ?? null;

  const run = async (action: () => Promise<void>) => {
    setMenu(false);
    setBusy(true);
    setProblem(null);
    try {
      await action();
      setMode('view');
    } catch (e) {
      setProblem(String(e));
    } finally {
      setBusy(false);
    }
  };

  const meta = [
    source.hint,
    t('panel.servers', { n: source.servers.length }),
    source.usedBytes != null
      ? source.totalBytes
        ? t('panel.trafficOf', { used: gib(i18n, source.usedBytes), total: gib(i18n, source.totalBytes) })
        : t('panel.traffic', { used: gib(i18n, source.usedBytes) })
      : null,
    source.expires ? t('panel.until', { date: date(i18n, source.expires) }) : null,
    source.updated ? ago(i18n, source.updated) : null,
  ].filter(Boolean);

  return (
    <section className={`source${collapsed ? ' collapsed' : ''}`}>
      <div className="source-head">
        {mode === 'rename' ? (
          <form
            className="rename"
            onSubmit={(e: FormEvent) => {
              e.preventDefault();
              run(() => onRename(source.id, name));
            }}
          >
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => e.key === 'Escape' && setMode('view')}
              ref={renameInput}
              maxLength={64}
              autoFocus
              aria-label={t('panel.newName')}
            />
            <button type="submit" disabled={busy || !name.trim()}>OK</button>
            <button type="button" onClick={() => setMode('view')}>{t('btn.cancel')}</button>
          </form>
        ) : (
          <button className="source-toggle" onClick={onToggle} aria-expanded={!collapsed} aria-controls={`servers-${source.id}`}>
            <ChevronDown size={16} strokeWidth={1.8} className="chev" />
            {source.kind === 'key' ? <KeyRound size={16} strokeWidth={1.7} /> : <Link2 size={16} strokeWidth={1.7} />}
            <span className="source-name">{source.name}</span>
          </button>
        )}
        {/* Пока переименовываем — значок и «⋮» прячем: в узкой панели они съедали поле ввода. */}
        {mode !== 'rename' && <span className="badge">{source.kind === 'key' ? t('panel.key') : t('panel.subscription')}</span>}
        {busy && <RefreshCw size={15} className="spin" aria-label={t('panel.updating')} />}
        {mode !== 'rename' && (
          <button className="icon-btn small" aria-label={t('panel.actions', { name: source.name })} aria-expanded={menu} onClick={() => setMenu(!menu)}>
            <EllipsisVertical size={18} strokeWidth={1.6} />
          </button>
        )}
        {menu && (
          <div className="menu source-menu" role="menu">
            {source.kind === 'subscription' && (
              <button role="menuitem" onClick={() => run(() => onRefresh(source.id))}>{t('panel.refresh')}</button>
            )}
            <button role="menuitem" onClick={() => run(() => onProbe(source.id))}>{t('panel.probe')}</button>
            <button role="menuitem" onClick={() => { setMenu(false); setName(source.name); setMode('rename'); }}>{t('panel.rename')}</button>
            <button role="menuitem" className="bad" onClick={() => { setMenu(false); setMode('remove'); }}>{t('panel.remove')}</button>
          </div>
        )}
      </div>
      <div className="source-meta">{meta.join(' · ')}</div>
      {mode === 'remove' && (
        <div className="confirm">
          {t('panel.confirmRemove', { name: source.name })}
          <div>
            <button className="bad" disabled={busy || locked} onClick={() => run(() => onRemove(source.id))}>{t('panel.remove')}</button>
            <button onClick={() => setMode('view')}>{t('btn.cancel')}</button>
          </div>
          {locked && <div className="muted">{t('panel.disconnectFirst')}</div>}
        </div>
      )}
      {(problem || source.problem) && <div className="source-meta bad">{problem ?? source.problem}</div>}

      <div role="radiogroup" aria-label={t('panel.serversOf', { name: source.name })} id={`servers-${source.id}`}>
        {collapsed && selected && (
          <ServerRow server={selected} probe={probes[selected.key]} checked locked={selectLocked} onSelect={onSelect} />
        )}
        {!collapsed && groups.map(([group, { icon, servers }]) => (
          <div key={group} className="group">
            {group && (
              <h3>
                {icon && <span className="group-icon">{icon}</span>}
                {group === COUNTRIES ? t('panel.countries') : group}
              </h3>
            )}
            {servers.map((s) => (
              <ServerRow key={s.key} server={s} probe={probes[s.key]} checked={s.key === selectedKey} locked={selectLocked} onSelect={onSelect} />
            ))}
          </div>
        ))}
      </div>
    </section>
  );
}

function ServerRow({
  server: s,
  probe,
  checked,
  locked,
  onSelect,
}: {
  server: Server;
  probe?: ProbeResult;
  checked: boolean;
  locked: boolean;
  onSelect(key: string): void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const n = parseServerName(s.name);
  return (
    <div className={`server${checked ? ' checked' : ''}${s.problem ? ' broken' : ''}`}>
      <label title={s.problem ?? s.name}>
        <input type="radio" name="server" checked={checked} disabled={!!s.problem || locked} onChange={() => onSelect(s.key)} />
        <span className="radio" aria-hidden="true" />
        {n.country ? <Flag code={n.country} className="mini-flag" /> : <span className="mini-icon">{n.groupIcon ?? '🌐'}</span>}
        <span className="server-text">
          <span className="server-title">
            {n.title}
            {n.note && <span className="server-note"> · {n.note}</span>}
          </span>
          <span className="server-meta">{s.problem ? t('panel.unsuitable') : transportOf(s.summary)}</span>
        </span>
      </label>
      {probe && <Latency probe={probe} />}
      <button className="icon-btn small" aria-label={t('panel.serverDetails', { name: n.title })} aria-expanded={open} onClick={() => setOpen(!open)}>
        <EllipsisVertical size={18} strokeWidth={1.6} />
      </button>
      {open && (
        <div className="server-details">
          <div>{s.name}</div>
          {s.problem ? <div className="bad">{s.problem}</div> : <div>{t('panel.engine', { summary: s.summary, engine: s.engine })}</div>}
          {s.warnings.map((w) => (
            <div key={w} className="muted">! {w}</div>
          ))}
        </div>
      )}
    </div>
  );
}

/** Значок задержки: зелёный — быстро, жёлтый — терпимо, красный — медленно. */
function Latency({ probe }: { probe: ProbeResult }) {
  const { t } = useI18n();
  if (probe.ms == null) {
    return <span className="latency fail" title={probe.error ?? ''}>{t('panel.noAnswer')}</span>;
  }
  const level = probe.ms < 300 ? 'good' : probe.ms < 800 ? 'ok' : 'slow';
  return <span className={`latency ${level}`} title={t('panel.latency')}>{t('panel.ms', { n: probe.ms })}</span>;
}

/** Добавление: ссылку проверяем сразу (скачиваем подписку / разбираем ключ), только потом сохраняем. */
function AddForm({ onAdd, onDone }: { onAdd: Props['onAdd']; onDone(): void }) {
  const { t } = useI18n();
  const [link, setLink] = useState('');
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setProblem(null);
    try {
      await onAdd(name.trim() || null, link.trim());
      onDone();
    } catch (err) {
      setProblem(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="add-form" onSubmit={submit}>
      <label>
        {t('add.link')}
        <textarea
          value={link}
          onChange={(e) => setLink(e.target.value)}
          placeholder={t('add.linkPlaceholder')}
          rows={3}
          autoFocus
          spellCheck={false}
        />
      </label>
      <label>
        {t('add.name')} <span className="muted">{t('add.optional')}</span>
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder={t('add.namePlaceholder')} maxLength={64} />
      </label>
      <p className="muted small">{t('add.privacy')}</p>
      {problem && <p className="bad small">{problem}</p>}
      <div className="form-buttons">
        <button type="button" onClick={onDone} disabled={busy}>{t('btn.cancel')}</button>
        <button type="submit" className="accent" disabled={busy || !link.trim()}>
          {busy ? t('add.checking') : t('add.submit')}
        </button>
      </div>
    </form>
  );
}
