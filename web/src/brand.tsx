import { createContext, ReactNode, useCallback, useContext, useEffect, useState } from "react";

interface BrandCtx {
  name: string;
  setName: (n: string) => void;
}

const DEFAULT = "Skien Monitoring";
const Ctx = createContext<BrandCtx>({ name: DEFAULT, setName: () => {} });
export const useBrand = () => useContext(Ctx);

/** Product name (editable in Settings → General); public so the sign-in page can show it. */
export function BrandProvider({ children }: { children: ReactNode }) {
  const [name, setNameState] = useState<string>(() => {
    try {
      return localStorage.getItem("vigil.brand") || DEFAULT;
    } catch {
      return DEFAULT;
    }
  });
  const setName = useCallback((n: string) => {
    setNameState(n);
    try {
      localStorage.setItem("vigil.brand", n);
    } catch {
      /* ignore */
    }
  }, []);
  useEffect(() => {
    fetch("/api/branding", { credentials: "same-origin" })
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => d?.name && setName(d.name))
      .catch(() => {});
  }, [setName]);
  useEffect(() => {
    document.title = name;
  }, [name]);
  return <Ctx.Provider value={{ name, setName }}>{children}</Ctx.Provider>;
}
