export type AppPage = "home" | "history" | "settings" | "about";

export type TransitionPhase = "idle" | "exiting" | "entering";

export const PAGE_EXIT_MS = 150;
export const PAGE_ENTER_MS = 200;
export const PAGE_TRANSITION_TIMEOUT_PAD_MS = 50;

export type PageTransitionState = {
  displayedPage: AppPage;
  phase: TransitionPhase;
  pendingPage: AppPage | null;
};

export function createPageTransitionState(
  initial: AppPage,
): PageTransitionState {
  return { displayedPage: initial, phase: "idle", pendingPage: null };
}

export type PageTransitionEvent =
  | { type: "target"; page: AppPage }
  | { type: "exitDone" }
  | { type: "enterDone" }
  | { type: "skipAnimations"; page: AppPage };

export function reducePageTransition(
  state: PageTransitionState,
  event: PageTransitionEvent,
): PageTransitionState {
  switch (event.type) {
    case "skipAnimations":
      return {
        displayedPage: event.page,
        phase: "idle",
        pendingPage: null,
      };

    case "target": {
      if (state.phase === "idle") {
        if (event.page === state.displayedPage) return state;
        return {
          ...state,
          phase: "exiting",
          pendingPage: event.page,
        };
      }
      // exiting | entering: only update latest intent
      if (event.page === state.displayedPage && state.phase === "entering") {
        // navigating back to the page we are entering — clear pending
        return { ...state, pendingPage: null };
      }
      return { ...state, pendingPage: event.page };
    }

    case "exitDone": {
      if (state.phase !== "exiting" || state.pendingPage === null) {
        return state;
      }
      // Cancelled back to the page still on screen — finish without re-enter.
      if (state.pendingPage === state.displayedPage) {
        return { ...state, phase: "idle", pendingPage: null };
      }
      return {
        displayedPage: state.pendingPage,
        phase: "entering",
        pendingPage: null,
      };
    }

    case "enterDone": {
      if (state.phase !== "entering") return state;
      if (
        state.pendingPage !== null &&
        state.pendingPage !== state.displayedPage
      ) {
        return {
          ...state,
          phase: "exiting",
          // pendingPage kept
        };
      }
      return { ...state, phase: "idle", pendingPage: null };
    }
  }
}
