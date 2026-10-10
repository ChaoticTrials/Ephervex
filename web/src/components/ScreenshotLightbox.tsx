import {useCallback, useEffect, useRef, useState, type PointerEvent, type ReactNode} from 'react';

import styles from './ScreenshotLightbox.module.css';

export type LightboxSlide = {
  key: string;
  title: string;
  src: string;
  width: number;
  height: number;
};

type Props = {
  slides: LightboxSlide[];
  index: number;
  onIndexChange: (index: number) => void;
  onClose: () => void;
};

/** Horizontal travel, in pixels, that turns a drag into a swipe. */
const SWIPE_DISTANCE = 60;
/** Travel before a touch counts as a drag rather than a tap. */
const DRAG_START = 10;

/** A full-size viewer with previous and next controls, arrow keys, and swipe. */
export default function ScreenshotLightbox({
  slides,
  index,
  onIndexChange,
  onClose,
}: Props): ReactNode {
  const dialog = useRef<HTMLDivElement>(null);
  const start = useRef<{x: number; y: number; captured: boolean} | null>(null);
  const dragged = useRef(false);
  const previousIndex = useRef(index);
  const [dragX, setDragX] = useState(0);
  const slide = slides[index];
  const hasPrevious = index > 0;
  const hasNext = index < slides.length - 1;
  const direction = index === previousIndex.current ? 0 : index > previousIndex.current ? 1 : -1;

  useEffect(() => {
    previousIndex.current = index;
  }, [index]);

  const go = useCallback(
    (step: number) => {
      const target = index + step;
      if (target >= 0 && target < slides.length) {
        onIndexChange(target);
      }
    },
    [index, slides.length, onIndexChange],
  );

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const scrollLock = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    dialog.current?.focus();
    return () => {
      document.body.style.overflow = scrollLock;
      opener?.focus?.();
    };
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        onClose();
      } else if (event.key === 'ArrowLeft') {
        go(-1);
      } else if (event.key === 'ArrowRight') {
        go(1);
      } else if (event.key === 'Tab') {
        // Keep keyboard focus inside the viewer while it is open.
        const controls = dialog.current?.querySelectorAll<HTMLElement>('button:not(:disabled), a[href]');
        if (!controls || controls.length === 0) {
          return;
        }
        const first = controls[0];
        const last = controls[controls.length - 1];
        const active = document.activeElement;
        // A control that just became disabled drops focus to the page behind.
        const outside = !dialog.current?.contains(active) || active === dialog.current;
        if (event.shiftKey && (active === first || outside)) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && (active === last || outside)) {
          event.preventDefault();
          first.focus();
        }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [go, onClose]);

  // Fetch the neighbors early so a swipe does not wait on the network.
  useEffect(() => {
    for (const neighbor of [slides[index - 1], slides[index + 1]]) {
      if (neighbor) {
        new Image().src = neighbor.src;
      }
    }
  }, [index, slides]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.pointerType === 'mouse') {
      return;
    }
    start.current = {x: event.clientX, y: event.clientY, captured: false};
    dragged.current = false;
  };
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (!start.current) {
      return;
    }
    const dx = event.clientX - start.current.x;
    const dy = event.clientY - start.current.y;
    if (Math.abs(dx) > Math.abs(dy) && Math.abs(dx) > DRAG_START) {
      // Capture only once a drag begins, so a tap still reaches the control it hit.
      if (!start.current.captured) {
        start.current.captured = true;
        event.currentTarget.setPointerCapture(event.pointerId);
      }
      dragged.current = true;
      // Resist dragging past the first or last screenshot.
      const blocked = (dx > 0 && !hasPrevious) || (dx < 0 && !hasNext);
      setDragX(blocked ? dx / 4 : dx);
    } else if (Math.abs(dy) > Math.abs(dx)) {
      setDragX(0);
    }
  };
  const onPointerEnd = (event: PointerEvent<HTMLDivElement>) => {
    if (!start.current) {
      return;
    }
    const dx = event.clientX - start.current.x;
    start.current = null;
    setDragX(0);
    if (event.type === 'pointerup' && Math.abs(dx) >= SWIPE_DISTANCE) {
      go(dx < 0 ? 1 : -1);
    }
  };

  if (!slide) {
    return null;
  }

  return (
    <div
      ref={dialog}
      className={styles.backdrop}
      role="dialog"
      aria-modal="true"
      aria-label="Screenshot viewer"
      tabIndex={-1}
      onClick={(event) => {
        if (event.target === event.currentTarget) {
          onClose();
        }
      }}>
      <header className={styles.bar}>
        <span className={styles.caption} aria-live="polite">
          <strong>{slide.title}</strong>
          <span>
            {index + 1} of {slides.length}
          </span>
        </span>
        <a className={styles.action} href={slide.src} target="_blank" rel="noreferrer">
          Open original
        </a>
        <button type="button" className={styles.action} onClick={onClose} aria-label="Close viewer">
          Close
        </button>
      </header>
      <div
        className={styles.stage}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerEnd}
        onPointerCancel={onPointerEnd}
        onClick={(event) => {
          if (event.target === event.currentTarget && !dragged.current) {
            onClose();
          }
        }}>
        <button
          type="button"
          className={`${styles.nav} ${styles.previous}`}
          onClick={() => go(-1)}
          disabled={!hasPrevious}
          aria-label="Previous screenshot">
          <span aria-hidden="true">&#8249;</span>
        </button>
        <img
          key={slide.key}
          className={`${styles.image} ${direction > 0 ? styles.fromRight : direction < 0 ? styles.fromLeft : ''}`}
          style={dragX ? {transform: `translateX(${dragX}px)`, transition: 'none'} : undefined}
          src={slide.src}
          width={slide.width}
          height={slide.height}
          alt={slide.title}
          draggable={false}
        />
        <button
          type="button"
          className={`${styles.nav} ${styles.next}`}
          onClick={() => go(1)}
          disabled={!hasNext}
          aria-label="Next screenshot">
          <span aria-hidden="true">&#8250;</span>
        </button>
      </div>
    </div>
  );
}
