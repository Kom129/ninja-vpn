// «С телефона»: QR-код страницы в домашней сети. Телефон открывает её камерой, вставляет
// ключ или подписку — они появляются в «Профилях». Страница живёт, пока открыто это окно
// (и не дольше 10 минут — так решает мотор, motor/src/pair.rs).

import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, X } from 'lucide-react';
import { backend, type Source } from '../api';
import { useI18n } from '../i18n';

export function PhonePairDialog({ onAdded, onClose }: { onAdded: (source: Source) => void; onClose: () => void }) {
  const { t } = useI18n();
  const [page, setPage] = useState<{ url: string; qrSvg: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState<string[]>([]);

  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    backend.onPairAdded((source) => {
      if (!alive) return;
      setAdded((list) => [...list, source.name]);
      onAdded(source);
    }).then((u) => (alive ? (unlisten = u) : u()));
    backend.pairStart().then(
      (p) => alive && setPage(p),
      (e) => alive && setError(String(e)),
    );
    return () => {
      alive = false;
      unlisten?.();
      backend.pairStop().catch(() => {});
    };
    // onAdded меняется на каждой отрисовке окна — страницу из-за этого не перезапускаем.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Окно — в <body>: у панели «Профили» размытие фона, внутри неё fixed уезжает.
  return createPortal(
    <div className="picker-backdrop" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="picker phone-dialog" role="dialog" aria-modal="true" aria-labelledby="phone-title">
        <div className="picker-head">
          <h2 id="phone-title">{t('phone.title')}</h2>
          <button className="icon-btn" aria-label={t('menu.close')} onClick={onClose}>
            <X size={20} strokeWidth={1.6} />
          </button>
        </div>
        {error ? (
          <p className="app-note bad">{t('phone.error', { why: error })}</p>
        ) : (
          <div className="phone-body">
            <div className="phone-qr" aria-label={page?.url ?? ''} dangerouslySetInnerHTML={page ? { __html: page.qrSvg } : undefined} />
            <div className="phone-steps">
              <ol>
                <li>{t('phone.step1')}</li>
                <li>{t('phone.step2')}</li>
                <li>{t('phone.step3')}</li>
              </ol>
              {page && (
                <p className="muted small">
                  {t('phone.address')} <span className="phone-url">{page.url}</span>
                </p>
              )}
            </div>
          </div>
        )}
        {added.length > 0 ? (
          <ul className="phone-added">
            {added.map((name, i) => (
              <li key={i}>
                <Check size={16} strokeWidth={2} aria-hidden="true" /> {t('phone.added', { name })}
              </li>
            ))}
          </ul>
        ) : (
          !error && page && <p className="phone-waiting">{t('phone.waiting')}</p>
        )}
        {!error && (
          <p className="muted small phone-notes">
            {t('phone.firewall')} {t('phone.lanNote')}
          </p>
        )}
        <div className="form-buttons">
          <button className={added.length ? 'accent' : ''} onClick={onClose}>
            {added.length ? t('phone.done') : t('btn.cancel')}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
