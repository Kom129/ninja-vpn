// Режим «Приложения»: полоса выбранных программ под сценой и окно «Добавить программу».
// У каждой программы — значок, состояние от сторожа (через VPN / напрямую / не запущена)
// и меню: запустить или перезапустить через VPN, закрыть, убрать из списка.

import { useEffect, useMemo, useRef, useState } from 'react';
import { FolderOpen, Plus, Search, X } from 'lucide-react';
import type { AppItem, AppStatus, Program } from '../api';
import { useI18n, type I18n } from '../i18n';

/** Как показать состояние программы: цвет точки и подпись. */
export type Tone = 'ok' | 'wait' | 'warn' | 'bad' | 'idle';

export function describe(app: AppItem, status: AppStatus | undefined, connected: boolean, t: I18n['t']): { tone: Tone; text: string } {
  if (app.problem) return { tone: 'bad', text: t('app.notFound') };
  if (!status || status.processes === 0) {
    return { tone: 'idle', text: connected ? t('app.notRunning') : app.autostart ? t('app.willStart') : t('app.notRunning') };
  }
  if (status.directTotal > 0) {
    return { tone: 'bad', text: status.ours > 0 ? t('app.partlyDirect') : t('app.direct') };
  }
  if (status.ours === 0) return { tone: 'warn', text: t('app.notOurs') };
  if (!connected) return { tone: 'wait', text: t('app.waiting') };
  if (!status.portOk) return { tone: 'warn', text: t('app.needsRestart') };
  return { tone: 'ok', text: status.viaVpn > 0 ? t('app.viaVpnCount', { n: status.viaVpn }) : t('app.viaVpn') };
}

/** Значок программы или первая буква названия, если значка нет. */
export function AppIcon({ icon, name, size = 30 }: { icon: string | null; name: string; size?: number }) {
  return icon ? (
    <img className="app-icon" src={icon} alt="" width={size} height={size} draggable={false} />
  ) : (
    <span className="app-icon letter" style={{ width: size, height: size, fontSize: size * 0.48 }} aria-hidden="true">
      {name.trim().charAt(0).toUpperCase() || '?'}
    </span>
  );
}

interface BarProps {
  /** Режим «все через VPN, кроме этих»: программы списка — исключения, ходят напрямую. */
  except: boolean;
  apps: AppItem[];
  statuses: Record<string, AppStatus>;
  connected: boolean;
  onAdd: () => void;
  onLaunch: (id: string) => Promise<void>;
  onRestart: (id: string) => Promise<void>;
  onClose: (id: string) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
  onAutostart: (id: string, on: boolean) => Promise<void>;
}

export function AppsBar({ except, apps, statuses, connected, onAdd, ...actions }: BarProps) {
  const { t } = useI18n();
  // Переключатель «открывать при подключении» — прямо на карточке: в меню программы его
  // мало кто находил. Пока сохраняется, показываем новое положение сразу.
  const [pending, setPending] = useState<Record<string, boolean>>({});
  const toggleAutostart = (app: AppItem, on: boolean) => {
    setPending((p) => ({ ...p, [app.id]: on }));
    actions.onAutostart(app.id, on).finally(() => setPending(({ [app.id]: _, ...rest }) => rest));
  };
  const [open, setOpen] = useState<string | null>(null);
  const opened = apps.find((a) => a.id === open) ?? null;
  return (
    <div className="apps-bar">
      <div className="apps-row" role="list" aria-label={t('apps.listAria')}>
        {apps.map((app) => {
          const { tone, text } = except ? { tone: 'idle' as Tone, text: t('app.exception') } : describe(app, statuses[app.id], connected, t);
          const auto = pending[app.id] ?? app.autostart;
          return (
            <div key={app.id} role="listitem" className={`app-chip-wrap${except ? '' : ' with-switch'}`}>
            <button
              className={`app-chip${open === app.id ? ' open' : ''}`}
              aria-expanded={open === app.id}
              onClick={() => setOpen(open === app.id ? null : app.id)}
            >
              <AppIcon icon={app.icon} name={app.name} />
              <span className="app-chip-text">
                <span className="app-chip-name">{app.name}</span>
                <span className={`app-chip-state ${tone}`}>
                  <span className="dot" aria-hidden="true" />
                  <span className="state-text">{text}</span>
                </span>
              </span>
            </button>
            {!except && (
              <button
                className="auto-switch"
                role="switch"
                aria-checked={auto}
                aria-label={t('app.autostartAria', { name: app.name })}
                title={t('app.autostartAria', { name: app.name })}
                onClick={() => toggleAutostart(app, !auto)}
              >
                <span className="knob" aria-hidden="true" />
              </button>
            )}
            </div>
          );
        })}
        <button className="app-chip add-chip" onClick={onAdd}>
          <Plus size={18} strokeWidth={1.8} /> {apps.length ? t('apps.add') : except ? t('apps.addException') : t('apps.addApp')}
        </button>
      </div>
      {opened && (
        <AppMenu
          key={opened.id}
          app={opened}
          except={except}
          status={statuses[opened.id]}
          connected={connected}
          onDone={() => setOpen(null)}
          {...actions}
        />
      )}
    </div>
  );
}

interface MenuProps extends Omit<BarProps, 'apps' | 'statuses' | 'onAdd' | 'except'> {
  except: boolean;
  app: AppItem;
  status: AppStatus | undefined;
  onDone: () => void;
}

function AppMenu({ except, app, status, connected, onDone, onLaunch, onRestart, onClose, onRemove, onAutostart }: MenuProps) {
  const { t } = useI18n();
  const [confirm, setConfirm] = useState<'restart' | 'close' | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const running = !!status && status.processes > 0;
  const { tone, text } = except ? { tone: 'idle' as Tone, text: t('app.exception') } : describe(app, status, connected, t);

  const run = async (action: () => Promise<void>, close = true) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      if (close) onDone();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setConfirm(null);
    }
  };

  return (
    <div className="app-menu" role="dialog" aria-label={app.name}>
      <div className="app-menu-head">
        <AppIcon icon={app.icon} name={app.name} size={40} />
        <div className="app-menu-title">
          <b>{app.name}</b>
          <span className={`app-chip-state ${tone}`}>
            <span className="dot" aria-hidden="true" />
            {text}
          </span>
        </div>
        <button className="icon-btn small" aria-label={t('menu.close')} onClick={onDone}>
          <X size={16} />
        </button>
      </div>

      {except && (
        <p className="app-note">{t('menu.exceptNote')}</p>
      )}
      {except && app.exe && <p className="app-path" title={app.exe}>{app.exe}</p>}
      {except && error && <p className="app-note bad">{error}</p>}
      {except && (
        <button className="link remove-link" disabled={busy} onClick={() => run(() => onRemove(app.id))}>
          {t('menu.removeException')}
        </button>
      )}
      {!except && (<>
      {status && status.directTotal > 0 && (
        <p className="app-note bad">
          {t('menu.directCount', { n: status.directTotal, list: status.direct.join(', ') })}{' '}
          {status.ours > 0 ? t('menu.directPartly') : t('menu.directNotOurs')}
        </p>
      )}
      {running && status!.ours === 0 && status!.directTotal === 0 && (
        <p className="app-note">{t('menu.openedByItself')}</p>
      )}
      {!connected && running && status!.ours > 0 && (
        <p className="app-note">{t('menu.waitingVpn')}</p>
      )}

      {confirm ? (
        <div className="confirm">
          {confirm === 'restart' ? t('menu.confirmRestart') : t('menu.confirmClose')}
          <div>
            <button
              className="bad"
              disabled={busy}
              onClick={() => run(() => (confirm === 'restart' ? onRestart(app.id) : onClose(app.id)))}
            >
              {busy ? t('menu.wait') : confirm === 'restart' ? t('menu.restart') : t('menu.closeIt')}
            </button>
            <button disabled={busy} onClick={() => setConfirm(null)}>
              {t('btn.cancel')}
            </button>
          </div>
        </div>
      ) : (
        <div className="app-menu-actions">
          {connected && !running && !app.problem && (
            <button className="accent" disabled={busy} onClick={() => run(() => onLaunch(app.id))}>
              {t('menu.launch')}
            </button>
          )}
          {connected && running && (tone !== 'ok' || status!.ours === 0) && (
            <button className="accent" disabled={busy} onClick={() => setConfirm('restart')}>
              {t('menu.restartVpn')}
            </button>
          )}
          {running && (
            <button disabled={busy} onClick={() => setConfirm('close')}>
              {t('menu.closeProgram')}
            </button>
          )}
          {!connected && !running && <p className="app-note">{t(app.autostart ? 'menu.connectFirst' : 'menu.connectThenLaunch')}</p>}
        </div>
      )}

      <label className="toggle small-toggle">
        <input
          type="checkbox"
          checked={app.autostart}
          onChange={(e) => run(() => onAutostart(app.id, e.target.checked), false)}
        />
        <span>{t('menu.autostart')}</span>
      </label>

      <p className="app-how">
        {app.support === 'chromium' ? t('menu.howChromium') : t('menu.howOther')}
      </p>
      {app.exe && <p className="app-path" title={app.exe}>{app.exe}</p>}
      {error && <p className="app-note bad">{error}</p>}

      <button className="link remove-link" disabled={busy} onClick={() => run(() => onRemove(app.id))}>
        {t('menu.remove')}
      </button>
      </>)}
    </div>
  );
}

interface PickerProps {
  apps: AppItem[];
  load: () => Promise<Program[]>;
  pick: () => Promise<Program | null>;
  onAdd: (p: Program) => Promise<void>;
  onClose: () => void;
}

/** Окно «Добавить программу»: программы из меню «Пуск» с поиском и выбор файла вручную. */
export function AppPicker({ apps, load, pick, onAdd, onClose }: PickerProps) {
  const { t } = useI18n();
  const [programs, setPrograms] = useState<Program[] | null>(null);
  const [query, setQuery] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState<string | null>(null);
  const search = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let cancelled = false;
    load().then(
      (list) => !cancelled && setPrograms(list),
      (e) => !cancelled && setError(String(e)),
    );
    search.current?.focus();
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onKey);
    return () => {
      cancelled = true;
      window.removeEventListener('keydown', onKey);
    };
  }, [load, onClose]);

  const added = useMemo(() => new Set(apps.map((a) => (a.exe ?? '').toLowerCase())), [apps]);
  const shown = (programs ?? []).filter((p) => p.name.toLowerCase().includes(query.trim().toLowerCase()));

  const add = async (p: Program) => {
    setAdding(p.exe);
    setError(null);
    try {
      await onAdd(p);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setAdding(null);
    }
  };

  const fromFile = async () => {
    setError(null);
    try {
      const p = await pick();
      if (p) await add(p);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="picker-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="picker" role="dialog" aria-label={t('picker.aria')}>
        <div className="picker-head">
          <h2>{t('picker.title')}</h2>
          <button className="icon-btn" aria-label={t('win.close')} onClick={onClose}>
            <X size={18} />
          </button>
        </div>
        <label className="picker-search">
          <Search size={16} />
          <input ref={search} value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t('picker.search')} />
        </label>
        <div className="picker-list">
          {programs === null && !error && <p className="panel-note">{t('picker.loading')}</p>}
          {programs !== null && shown.length === 0 && <p className="panel-note">{t('picker.nothing')}</p>}
          {shown.map((p) => {
            const already = added.has(p.exe.toLowerCase());
            return (
              <button key={p.exe} className="picker-item" disabled={already || adding !== null} onClick={() => add(p)}>
                <AppIcon icon={p.icon} name={p.name} size={32} />
                <span className="picker-name">{p.name}</span>
                <span className={`badge${p.support === 'chromium' ? ' good' : ''}`}>
                  {already ? t('picker.inList') : adding === p.exe ? t('picker.adding') : p.support === 'chromium' ? t('picker.reliable') : t('picker.checkLater')}
                </span>
              </button>
            );
          })}
        </div>
        {error && <p className="app-note bad">{error}</p>}
        <div className="picker-foot">
          <button className="refresh" onClick={fromFile} disabled={adding !== null}>
            <FolderOpen size={16} /> {t('picker.fromFile')}
          </button>
          <span className="muted small">{t('picker.reliableHint')}</span>
        </div>
      </div>
    </div>
  );
}
