// src/components/PageTransition.tsx
import { useEffect, useState, type ReactNode } from "react";
import { motion, useReducedMotion } from "motion/react";

export type AppPage = "home" | "history" | "settings" | "logs" | "about";
export type NonHomePage = Exclude<AppPage, "home">;

const EXIT_S = 0.15;
const ENTER_S = 0.2;

interface PageTransitionProps {
  page: AppPage;
  /** Home subtree: stays mounted (hidden + inert) across tab switches. */
  home: (active: boolean) => ReactNode;
  /** Renders the currently displayed non-home page. */
  children: (page: NonHomePage) => ReactNode;
}

/**
 * Container-level two-phase page transition (fade out -> swap -> fade in),
 * mirroring the previous hand-rolled PageShell timing. Home stays mounted;
 * the active flag tells it whether it is on screen.
 */
export function PageTransition({ page, home, children }: PageTransitionProps) {
  const [displayed, setDisplayed] = useState<AppPage>(page);
  const [visible, setVisible] = useState(true);
  const reduced = useReducedMotion();

  useEffect(() => {
    if (reduced) {
      setDisplayed(page);
      setVisible(true);
      return;
    }
    if (page !== displayed && visible) {
      setVisible(false);
    }
  }, [page, displayed, visible, reduced]);

  const homeActive = displayed === "home";

  return (
    <motion.div
      className="page-shell"
      initial={false}
      animate={{ opacity: visible ? 1 : 0, y: visible ? 0 : 4 }}
      transition={{
        duration: visible ? ENTER_S : EXIT_S,
        ease: visible ? "easeOut" : "easeIn",
      }}
      onAnimationComplete={() => {
        if (!visible) {
          if (displayed !== page) {
            setDisplayed(page);
          }
          setVisible(true);
        }
      }}
    >
      <div
        className={homeActive ? undefined : "page-hidden"}
        aria-hidden={homeActive ? undefined : true}
        {...(homeActive ? {} : { inert: true })}
      >
        {home(homeActive)}
      </div>
      {displayed !== "home" && children(displayed as NonHomePage)}
    </motion.div>
  );
}
