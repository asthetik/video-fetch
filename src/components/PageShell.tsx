import { useEffect, useState, type ReactNode, type TransitionEvent } from "react";
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

  useEffect(() => {
    if (reducedMotion) return;
    if (phase === "entering") {
      setEnterActive(false);
      const id = requestAnimationFrame(() => {
        setEnterActive(true);
      });
      return () => cancelAnimationFrame(id);
    }
    setEnterActive(false);
  }, [phase, reducedMotion]);

  function handleTransitionEnd(event: TransitionEvent<HTMLDivElement>) {
    if (event.target !== event.currentTarget) return;
    if (event.propertyName !== "opacity") return;
    if (phase === "exiting") onExitComplete();
    if (phase === "entering") onEnterComplete();
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
