/**
 * Collects unlisten functions whose registration is asynchronous.
 *
 * A plain `unlisteners.push(fn)` inside `.then()` loses the unlisten when the
 * promise resolves after the effect cleanup already ran: the listener stays
 * registered on the global event bridge, and every remount piles more on. The
 * bag calls such late arrivals immediately instead of storing them.
 */
export type Unlisten = () => void;

export interface ListenerBag {
  track: (fn: Unlisten) => void;
  dispose: () => void;
}

export function createListenerBag(): ListenerBag {
  let disposed = false;
  const unlisteners: Unlisten[] = [];
  return {
    track(fn) {
      if (disposed) {
        fn();
        return;
      }
      unlisteners.push(fn);
    },
    dispose() {
      disposed = true;
      for (const fn of unlisteners.splice(0)) {
        fn();
      }
    },
  };
}
