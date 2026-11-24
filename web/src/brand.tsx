import { createContext, ReactNode, useCallback, useContext, useEffect, useState } from "react";

export interface BrandInfo {
  name: string;
  company: string;
  version: string;
  build: string | null;
}

interface BrandCtx extends BrandInfo {
  setBrand: (b: Partial<BrandInfo>) => void;
}

const DEFAULTS: BrandInfo = { name: "MonitorLab", company: "Skien Software Lab", version: "", build: null };
const Ctx = createContext<BrandCtx>({ ...DEFAULTS, setBrand: () => {} });
export const useBrand = () => useContext(Ctx);

function cached(): BrandInfo {
  try {
    const v = localStorage.getItem("monitorlab.brandinfo");
    if (v) return { ...DEFAULTS, ...JSON.parse(v) };
  } catch {
    /* ignore */
  }
  return DEFAULTS;
}

/** Product / company name and version (editable in Settings → General); public so the sign-in page can show them. */
export function BrandProvider({ children }: { children: ReactNode }) {
  const [info, setInfo] = useState<BrandInfo>(cached);
  const setBrand = useCallback((b: Partial<BrandInfo>) => {
    setInfo((cur) => {
      const next = { ...cur, ...b };
      try {
        localStorage.setItem("monitorlab.brandinfo", JSON.stringify(next));
      } catch {
        /* ignore */
      }
      return next;
    });
  }, []);
  useEffect(() => {
    fetch("/api/branding", { credentials: "same-origin" })
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => d && setBrand({ name: d.name || DEFAULTS.name, company: d.company || DEFAULTS.company, version: d.version || "", build: d.build ?? null }))
      .catch(() => {});
  }, [setBrand]);
  useEffect(() => {
    document.title = info.name;
  }, [info.name]);
  return <Ctx.Provider value={{ ...info, setBrand }}>{children}</Ctx.Provider>;
}

/** "© 2026 Skien Software Lab" + "Version 1.0.0 · build abc1234". */
export function BrandFooter({ className = "" }: { className?: string }) {
  const { company, version, build } = useBrand();
  return (
    <div className={`brand-footer ${className}`}>
      <div>
        © {new Date().getFullYear()} {company}
      </div>
      {version && (
        <div>
          Version {version}
          {build && <span title="Build (commit)"> · build {build}</span>}
        </div>
      )}
    </div>
  );
}
