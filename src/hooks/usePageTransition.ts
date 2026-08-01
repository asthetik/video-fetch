import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import {
  PAGE_ENTER_MS,
  PAGE_EXIT_MS,
  PAGE_TRANSITION_TIMEOUT_PAD_MS,
  createPageTransitionState,
  reducePageTransition,
  type AppPage,
  type TransitionPhase,
} from "../lib/pageTransition";

function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() => {
    if (typeof window === "undefined" || !window.matchMedia) return false;
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  });

  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    const onChange = () => setReduced(mq.matches);
    onChange();
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  return reduced;
}

export function usePageTransition(targetPage: AppPage): {
  displayedPage: AppPage;
  phase: TransitionPhase;
  reducedMotion: boolean;
  onExitComplete: () => void;
  onEnterComplete: () => void;
} {
  const reducedMotion = usePrefersReducedMotion();
  const [state, dispatch] = useReducer(
    reducePageTransition,
    "home" as AppPage,
    createPageTransitionState,
  );
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    if (reducedMotion) {
      dispatch({ type: "skipAnimations", page: targetPage });
      return;
    }
    dispatch({ type: "target", page: targetPage });
  }, [targetPage, reducedMotion]);

  useEffect(() => {
    if (reducedMotion) return;
    if (state.phase !== "exiting" && state.phase !== "entering") return;
    const ms =
      (state.phase === "exiting" ? PAGE_EXIT_MS : PAGE_ENTER_MS) +
      PAGE_TRANSITION_TIMEOUT_PAD_MS;
    const id = window.setTimeout(() => {
      if (stateRef.current.phase === "exiting") {
        dispatch({ type: "exitDone" });
      } else if (stateRef.current.phase === "entering") {
        dispatch({ type: "enterDone" });
      }
    }, ms);
    return () => window.clearTimeout(id);
  }, [state.phase, state.displayedPage, reducedMotion]);

  const onExitComplete = useCallback(() => {
    dispatch({ type: "exitDone" });
  }, []);

  const onEnterComplete = useCallback(() => {
    dispatch({ type: "enterDone" });
  }, []);

  return {
    displayedPage: state.displayedPage,
    phase: state.phase,
    reducedMotion,
    onExitComplete,
    onEnterComplete,
  };
}
