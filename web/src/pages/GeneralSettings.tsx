import { useEffect, useState } from "react";
import { Activity } from "lucide-react";
import { api } from "../api";
import { useBrand } from "../brand";
import { Spinner, useToast } from "../components/ui";

export function GeneralSettings() {
  const { name, company, version, build, setBrand } = useBrand();
  const [value, setValue] = useState(name);
  const [comp, setComp] = useState(company);
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  useEffect(() => setValue(name), [name]);
  useEffect(() => setComp(company), [company]);
  const trimmed = value.trim();
  const compTrimmed = comp.trim();
  const dirty = trimmed !== name || compTrimmed !== company;
  const save = async () => {
    setBusy(true);
    try {
      await api("/branding", { method: "PUT", body: { name: trimmed, company: compTrimmed } });
      setBrand({ name: trimmed, company: compTrimmed });
      toast("Saved");
    } catch (e: any) {
      toast(e.message, true);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="page page-narrow">
      <div className="page-head">
        <div className="grow">
          <h1>General</h1>
          <div className="sub">Settings that apply to everyone using this hub</div>
        </div>
      </div>
      <div className="card card-pad col" style={{ gap: 20 }}>
        <div className="field">
          <label htmlFor="brand">Product name</label>
          <input id="brand" className="input" style={{ maxWidth: 420 }} maxLength={40} value={value} onChange={(e) => setValue(e.target.value)} onKeyDown={(e) => e.key === "Enter" && trimmed && save()} />
          <span className="hint">Shown in the header, on the sign-in page, in the browser tab and in alert emails and chat notifications.</span>
        </div>
        <div className="field">
          <label htmlFor="company">Company name</label>
          <input id="company" className="input" style={{ maxWidth: 420 }} maxLength={60} value={comp} onChange={(e) => setComp(e.target.value)} onKeyDown={(e) => e.key === "Enter" && trimmed && compTrimmed && save()} />
          <span className="hint">Shown in the footer: © {new Date().getFullYear()} {compTrimmed || "…"}</span>
        </div>
        <div className="field">
          <label>Preview</label>
          <div className="row brand-preview">
            <span className="brand-mark"><Activity size={19} /></span>
            <span style={{ fontSize: 20, fontWeight: 500 }}>{trimmed || "—"}</span>
          </div>
        </div>
        <div className="row" style={{ justifyContent: "flex-end" }}>
          <span className="small muted grow">Version {version}{build ? ` · build ${build}` : ""}</span>
          <button className="btn filled" disabled={busy || !trimmed || !compTrimmed || !dirty} onClick={save}>{busy && <Spinner />} Save</button>
        </div>
      </div>
    </div>
  );
}
