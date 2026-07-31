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

export function PageShell({
  phase,
  reducedMotion,
  onExitComplete,
  onEnterComplete,
  children,
}: PageShellProps) {
  const [enterActive, setEnterActive] = useState(false);
  const phaseTokenRef = useRef(0);
  const acceptedTokenRef = useRef<number | null>(null);

  useEffect(() => {
    if (reducedMotion) {
      acceptedTokenRef.current = null;
      setEnterActive(false);
      return;
    }

    if (phase === "entering") {
      const token = ++phaseTokenRef.current;
      // Disarm until enter-active so a late exit transitionend cannot complete enter.
      acceptedTokenRef.current = null;
      setEnterActive(false);
      const id = requestAnimationFrame(() => {
        if (phaseTokenRef.current !== token) return;
        setEnterActive(true);
        acceptedTokenRef.current = token;
      });
      return () => cancelAnimationFrame(id);
    }

    if (phase === "exiting") {
      const token = ++phaseTokenRef.current;
      acceptedTokenRef.current = token;
      setEnterActive(false);
      return;
    }

    acceptedTokenRef.current = null;
    setEnterActive(false);
  }, [phase, reducedMotion]);

  function handleTransitionEnd(event: TransitionEvent<HTMLDivElement>) {
    if (event.target !== event.currentTarget) return;
    if (event.propertyName !== "opacity") return;
    if (acceptedTokenRef.current !== phaseTokenRef.current) return;

    if (phase === "exiting") onExitComplete();
    if (phase === "entering" && enterActive) onEnterComplete();
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
