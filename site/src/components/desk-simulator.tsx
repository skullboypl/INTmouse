"use client";

import {
  useCallback,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
  type RefObject,
} from "react";
import styles from "./desk-simulator.module.css";

type Id = "a" | "b";

type Cursor = { screen: Id; x: number; y: number };

const ORDER: Id[] = ["a", "b"];

function clamp(n: number) {
  return Math.min(1, Math.max(0, n));
}

function Pointer({ x, y }: { x: number; y: number }) {
  return (
    <svg
      className={styles.pointer}
      style={{ ["--x" as string]: String(x), ["--y" as string]: String(y) }}
      viewBox="0 0 16 20"
      aria-hidden="true"
    >
      <path
        d="M1.2 1.2 5.6 17.2 8.1 10.4 14.4 8.2 1.2 1.2Z"
        fill="#141814"
        stroke="#f4f6f1"
        strokeWidth="1.2"
      />
    </svg>
  );
}

const START_CURSOR: Cursor = { screen: "a", x: 0.42, y: 0.48 };

export function DeskSimulator() {
  const aRef = useRef<HTMLDivElement>(null);
  const bRef = useRef<HTMLDivElement>(null);
  const cursorRef = useRef<Cursor>(START_CURSOR);
  const dragging = useRef(false);

  const [cursor, setCursor] = useState<Cursor>(START_CURSOR);
  const [fullscreen, setFullscreen] = useState(false);
  const [shiftOnly, setShiftOnly] = useState(false);
  const [deadCorners, setDeadCorners] = useState(false);
  const [relative, setRelative] = useState(false);
  const [blocked, setBlocked] = useState(false);
  const [hop, setHop] = useState(0);
  const [delta, setDelta] = useState({ x: 0, y: 0 });
  const [keyName, setKeyName] = useState<string | null>(null);

  const refs = { a: aRef, b: bRef };

  const allow = useCallback(
    (y: number, shift: boolean) => {
      if (fullscreen) return false;
      if (shiftOnly && !shift) return false;
      if (deadCorners && (y < 0.16 || y > 0.84)) return false;
      return true;
    },
    [fullscreen, shiftOnly, deadCorners],
  );

  const commit = useCallback((next: Cursor, denied: boolean, crossed: boolean) => {
    cursorRef.current = next;
    setCursor(next);
    if (crossed) {
      setHop((n) => n + 1);
      setBlocked(false);
    } else {
      setBlocked(denied);
    }
  }, []);

  const tryMove = useCallback(
    (next: Cursor, shift: boolean, dx = 0, dy = 0) => {
      let screen = next.screen;
      let x = next.x;
      const y = clamp(next.y);
      let crossed = false;
      let denied = false;

      if (x > 1 || x < 0) {
        const dir = x > 1 ? 1 : -1;
        const dest = ORDER[ORDER.indexOf(screen) + dir];
        if (dest && allow(y, shift)) {
          screen = dest;
          x = x > 1 ? x - 1 : x + 1;
          crossed = true;
        } else {
          if (dest) denied = true;
          x = clamp(x);
        }
      }

      if (dx || dy) setDelta({ x: dx, y: dy });
      commit({ screen, x: clamp(x), y }, denied, crossed);
    },
    [allow, commit],
  );

  function localHit(clientX: number, clientY: number): Cursor | null {
    for (const id of ORDER) {
      const el = refs[id].current;
      if (!el) continue;
      const r = el.getBoundingClientRect();
      if (r.width < 1 || r.height < 1) continue;
      if (clientX >= r.left && clientX <= r.right && clientY >= r.top && clientY <= r.bottom) {
        return {
          screen: id,
          x: (clientX - r.left) / r.width,
          y: (clientY - r.top) / r.height,
        };
      }
    }
    return null;
  }

  function onPointerMove(e: PointerEvent<HTMLDivElement>) {
    if (relative && dragging.current) {
      const el = refs[cursorRef.current.screen].current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      if (r.width < 1 || r.height < 1) return;
      tryMove(
        {
          screen: cursorRef.current.screen,
          x: cursorRef.current.x + e.movementX / r.width,
          y: cursorRef.current.y + e.movementY / r.height,
        },
        e.shiftKey,
        e.movementX,
        e.movementY,
      );
      return;
    }

    const hit = localHit(e.clientX, e.clientY);
    if (!hit) return;
    const cur = cursorRef.current;
    if (hit.screen !== cur.screen && !allow(hit.y, e.shiftKey)) {
      const pinned = { screen: cur.screen, x: cur.screen === "a" ? 1 : 0, y: clamp(hit.y) };
      commit(pinned, true, false);
      return;
    }
    const crossed = hit.screen !== cur.screen;
    commit(hit, false, crossed);
  }

  function onPointerDown(e: PointerEvent<HTMLDivElement>) {
    if (!relative) return;
    dragging.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
  }

  function onPointerUp(e: PointerEvent<HTMLDivElement>) {
    dragging.current = false;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  }

  function onKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    const step = 0.045;
    const cur = cursorRef.current;
    let x = cur.x;
    let y = cur.y;
    if (e.key === "ArrowRight") x += step;
    else if (e.key === "ArrowLeft") x -= step;
    else if (e.key === "ArrowUp") y -= step;
    else if (e.key === "ArrowDown") y += step;
    else {
      if (e.key === "Tab" || e.key === "Shift") return;
      setKeyName(e.key.length === 1 ? e.key.toUpperCase() : e.key);
      return;
    }
    e.preventDefault();
    const px = Math.round((x - cur.x) * 120);
    const py = Math.round((y - cur.y) * 80);
    tryMove({ screen: cur.screen, x, y }, e.shiftKey, px, py);
  }

  const status = blocked
    ? "Przejście wstrzymane"
    : cursor.screen === "a"
      ? "Sterujesz komputerem A"
      : "Sterujesz komputerem B";

  const hint = relative
    ? "Przytrzymaj przycisk i przeciągnij, tak jak w grze."
    : "Najedź na ekran albo kliknij biurko i użyj strzałek.";

  return (
    <figure className={styles.desk}>
      <figcaption className={styles.kicker}>
        <span>Symulator biurka</span>
        <span>tylko na tej stronie</span>
      </figcaption>

      <div
        className={styles.stage}
        tabIndex={0}
        role="application"
        aria-roledescription="symulator"
        aria-label="Dwa komputery na jednym biurku. Strzałki poruszają kursorem, a krawędź przenosi go na drugi komputer."
        onPointerMove={onPointerMove}
        onPointerDown={onPointerDown}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onKeyDown={onKeyDown}
      >
        <Machine
          id="a"
          label="Komputer A"
          active={cursor.screen === "a"}
          screenRef={aRef}
          deadCorners={deadCorners}
          cursor={cursor.screen === "a" ? cursor : null}
          delta={cursor.screen === "a" && relative ? delta : null}
          keyName={cursor.screen === "a" ? keyName : null}
        />
        <div className={styles.gap} aria-hidden="true">
          <span className={styles.lan}>sieć</span>
          {hop > 0 ? <i key={hop} className={styles.flash} /> : null}
        </div>
        <Machine
          id="b"
          label="Komputer B"
          laptop
          active={cursor.screen === "b"}
          screenRef={bRef}
          deadCorners={deadCorners}
          cursor={cursor.screen === "b" ? cursor : null}
          delta={cursor.screen === "b" && relative ? delta : null}
          keyName={cursor.screen === "b" ? keyName : null}
        />
      </div>

      <div className={styles.controls}>
        <Switch on={fullscreen} onClick={() => setFullscreen((v) => !v)} label="Blokada pełnego ekranu" />
        <Switch on={shiftOnly} onClick={() => setShiftOnly((v) => !v)} label="Tylko ze Shiftem" />
        <Switch on={deadCorners} onClick={() => setDeadCorners((v) => !v)} label="Narożniki zostają" />
        <Switch on={relative} onClick={() => setRelative((v) => !v)} label="Jak w grze" />
      </div>

      <p className={styles.hint}>
        {hint}
        {shiftOnly ? " Przejście wymaga wciśniętego Shifta." : ""}
        {fullscreen ? " Pełny ekran trzyma kursor na tym komputerze." : ""}
        {deadCorners ? " Góra i dół krawędzi nie puszczają kursora." : ""}
      </p>
      <p className={blocked ? `${styles.status} ${styles.statusBlocked}` : styles.status} aria-live="polite">
        {status}
      </p>
    </figure>
  );
}

function Machine({
  id,
  label,
  laptop,
  active,
  screenRef,
  deadCorners,
  cursor,
  delta,
  keyName,
}: {
  id: Id;
  label: string;
  laptop?: boolean;
  active: boolean;
  screenRef: RefObject<HTMLDivElement | null>;
  deadCorners: boolean;
  cursor: Cursor | null;
  delta: { x: number; y: number } | null;
  keyName: string | null;
}) {
  return (
    <div className={laptop ? `${styles.machine} ${styles.machineLaptop}` : styles.machine}>
      <div className={styles.bezel}>
        <div
          ref={screenRef}
          className={active ? styles.screen : `${styles.screen} ${styles.screenIdle}`}
        >
          <span className={styles.osd}>{label}</span>
          {deadCorners ? (
            <span className={id === "a" ? styles.cornersRight : styles.cornersLeft} />
          ) : null}
          {delta ? (
            <span className={styles.delta}>
              w bok {delta.x}, w górę {delta.y}
            </span>
          ) : null}
          {keyName ? <span className={styles.keychip}>{keyName}</span> : null}
          {cursor ? <Pointer x={cursor.x} y={cursor.y} /> : null}
        </div>
        <i className={active ? `${styles.led} ${styles.ledOn}` : styles.led} aria-hidden="true" />
      </div>
      {laptop ? <div className={styles.hinge} /> : (
        <>
          <div className={styles.neck} />
          <div className={styles.foot} />
        </>
      )}
    </div>
  );
}

function Switch({ on, onClick, label }: { on: boolean; onClick: () => void; label: string }) {
  return (
    <button type="button" role="switch" aria-checked={on} className={styles.switch} onClick={onClick}>
      <span className={styles.knob} aria-hidden="true" />
      {label}
    </button>
  );
}
