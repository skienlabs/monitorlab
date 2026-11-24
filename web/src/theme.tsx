import { createContext, ReactNode, useContext, useState } from "react";

/** Two themes: a colorful Light and a colorful AMOLED Black ("dark"). */
export type Mode = "light" | "dark";
interface ThemeCtx {
  mode: Mode;
  resolved: Mode;
  setMode: (m: Mode) => void;
}

const Ctx = createContext<ThemeCtx>({ mode: "light", resolved: "light", setMode: () => {} });
export const useTheme = () => useContext(Ctx);

function read(): Mode {
  try {
    const v = localStorage.getItem("monitorlab.theme");
    if (v === "light" || v === "dark") return v;
  } catch {
    /* ignore */
  }
  // First visit: follow the operating system once; afterwards the user's choice sticks.
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<Mode>(read);
  // Set synchronously during render so child effects (charts reading CSS variables) see the new theme.
  if (document.documentElement.dataset.theme !== mode) document.documentElement.dataset.theme = mode;
  const setMode = (m: Mode) => {
    setModeState(m);
    try {
      localStorage.setItem("monitorlab.theme", m);
    } catch {
      /* ignore */
    }
  };
  return <Ctx.Provider value={{ mode, resolved: mode, setMode }}>{children}</Ctx.Provider>;
}
