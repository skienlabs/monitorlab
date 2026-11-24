import { createContext, ReactNode, useContext, useEffect, useState } from "react";

type Mode = "light" | "dark" | "system";
interface ThemeCtx {
  mode: Mode;
  resolved: "light" | "dark";
  setMode: (m: Mode) => void;
}

const Ctx = createContext<ThemeCtx>({ mode: "system", resolved: "light", setMode: () => {} });
export const useTheme = () => useContext(Ctx);

function read(): Mode {
  try {
    const v = localStorage.getItem("monitorlab.theme");
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    /* ignore */
  }
  return "system";
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<Mode>(read);
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const [sysDark, setSysDark] = useState(mq.matches);
  useEffect(() => {
    const h = (e: MediaQueryListEvent) => setSysDark(e.matches);
    mq.addEventListener("change", h);
    return () => mq.removeEventListener("change", h);
  }, [mq]);
  const resolved: "light" | "dark" = mode === "system" ? (sysDark ? "dark" : "light") : mode;
  // Set synchronously during render so child effects (charts reading CSS variables) see the new theme.
  if (document.documentElement.dataset.theme !== resolved) document.documentElement.dataset.theme = resolved;
  const setMode = (m: Mode) => {
    setModeState(m);
    try {
      localStorage.setItem("monitorlab.theme", m);
    } catch {
      /* ignore */
    }
  };
  return <Ctx.Provider value={{ mode, resolved, setMode }}>{children}</Ctx.Provider>;
}
