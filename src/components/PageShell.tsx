import {
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type TransitionEvent,
} from "react";
import type { TransitionPhase } from "../lib/pageTransition";

interface PageShellProps {
  phase: TransitionPhase;
  reducedMotion: boolean;
  onExitComplete: () => void;
  onEnterComplete: () => void;
  children: ReactNode;
}

type ExpectedComplete = "exit" | "enter" | null;

export function PageShell({
  phase,
  reducedMotion,
  onExitComplete,
  onEnterComplete,
  children,
}: PageShellProps) {
  const [enterActive, setEnterActive] = useState(false);
  const phaseTokenRef = useRef(0);
  const expectedCompleteRef = useRef<ExpectedComplete>(null);

  useEffect(() => {
    if (reducedMotion) {
      expectedCompleteRef.current = null;
      setEnterActive(false);
      return;
    }

    if (phase === "entering") {
      const token = ++phaseTokenRef.current;
      // Do not accept completion until enter transition has actually started.
      expectedCompleteRef.current = null;
      setEnterActive(false);
      const id = requestAnimationFrame(() => {
        if (phaseTokenRef.current !== token) return;
        setEnterActive(true);
        expectedCompleteRef.current = "enter";
      });
      return () => cancelAnimationFrame(id);
    }

    if (phase === "exiting") {
      phaseTokenRef.current += 1;
      expectedCompleteRef.current = "exit";
      setEnterActive(false);
      return;
    }

    expectedCompleteRef.current = null;
    setEnterActive(false);
  }, [phase, reducedMotion]);

  function handleTransitionEnd(event: TransitionEvent<HTMLDivElement>) {
    if (event.target !== event.currentTarget) return;
    if (event.propertyName !== "opacity") return;

    const expected = expectedCompleteRef.current;
    if (expected === "exit" && phase === "exiting") {
      // Disarm immediately so a late exit event cannot complete a later enter.
      expectedCompleteRef.current = null;
      onExitComplete();
      return;
    }
    if (expected === "enter" && phase === "entering" && enterActive) {
      expectedCompleteRef.current = null;
      onEnterComplete();
    }
  }

  const className = [
    "page-shell",
    !reducedMotion && phase === "exiting" ? "page-shell--exit" : "",
    !reducedMotion && phase === "entering" ? "page-shell--enter" : "",
    !reducedMotion && phase === "entering" && enterActive
      ? "page-shell--enter-active"
      : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={className} onTransitionEnd={handleTransitionEnd}>
      {children}
    </div>
  );
}
