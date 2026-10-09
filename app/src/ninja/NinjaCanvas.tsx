// Холст со сценой ниндзя. Рисует только пока что-то движется, на скрытом окне останавливается.

import { useEffect, useRef } from 'react';
import { NinjaScene, type Phase } from './scene';
import { spritesReady } from './sprites';
import { useI18n } from '../i18n';

interface Props {
  phase: Phase;
  /** Где начинается нить слева (за кружком и его стрелкой) и где кончается справа. */
  insetLeft: number;
  insetRight: number;
  /** Высота нити от верха холста — центр кружков. */
  base: number;
}

export function NinjaCanvas({ phase, insetLeft, insetRight, base }: Props) {
  // Описание сцены для экранного диктора — на языке окна (scene.connected и т. д.).
  const { t } = useI18n();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const sceneRef = useRef(new NinjaScene());
  const kick = useRef<() => void>(() => {});

  useEffect(() => {
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext('2d')!;
    const scene = sceneRef.current;
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
    let width = 1, height = 1, ratio = 1, raf = 0, last = 0;

    const render = () => {
      ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
      scene.draw(ctx, { width, height, left: insetLeft, right: width - insetRight, base });
    };
    const frame = (now: number) => {
      raf = 0;
      if (document.hidden) {
        last = 0;
        return;
      }
      const dt = last ? Math.min((now - last) / 1000, 0.045) : 0;
      last = now;
      scene.update(dt);
      render();
      if (scene.animating) raf = requestAnimationFrame(frame);
      else last = 0;
    };
    const wake = () => {
      render();
      if (!raf && scene.animating && !document.hidden) raf = requestAnimationFrame(frame);
    };
    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      width = Math.max(1, rect.width);
      height = Math.max(1, rect.height);
      ratio = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
      render();
    };
    const onReduced = () => {
      scene.setReducedMotion(reduced.matches);
      wake();
    };

    scene.setReducedMotion(reduced.matches);
    kick.current = wake;
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    reduced.addEventListener('change', onReduced);
    document.addEventListener('visibilitychange', wake);
    resize();
    wake();
    // Кадры героини грузятся асинхронно — как только готовы, перерисовываем.
    spritesReady.then(() => wake());
    return () => {
      cancelAnimationFrame(raf);
      observer.disconnect();
      reduced.removeEventListener('change', onReduced);
      document.removeEventListener('visibilitychange', wake);
    };
  }, [insetLeft, insetRight, base]);

  useEffect(() => {
    sceneRef.current.setPhase(phase);
    kick.current();
  }, [phase]);

  return <canvas ref={canvasRef} className="ninja-canvas" role="img" aria-label={t(`scene.${phase}`)} />;
}
