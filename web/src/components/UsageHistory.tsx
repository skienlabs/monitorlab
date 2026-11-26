import { api } from "../api";
import { bytes } from "../format";
import { TimeChart } from "./Chart";
import { Seg, useLocalStorage, usePoll } from "./ui";
import { ChartCard } from "../pages/ServerPage";

export type UsageRange = "1h" | "6h" | "24h" | "3d" | "7d" | "30d";

interface Resp {
  ts: number[];
  series: { cpu: (number | null)[]; cpu_max: (number | null)[]; mem: (number | null)[]; mem_max: (number | null)[] };
}

/** CPU / memory history of a container, pod or group (stack, workload, namespace) over days. */
export function UsageHistory({
  serverId,
  kind,
  name,
  group,
  limits,
}: {
  serverId: string;
  kind: "container" | "pod";
  name?: string;
  group?: string;
  /** Pods: requests / limits drawn as reference values in the headline. */
  limits?: { cpu_req?: number; cpu_lim?: number; mem_req?: number; mem_lim?: number };
}) {
  const [range, setRange] = useLocalStorage<UsageRange>("monitorlab.usage.range", "6h");
  const q = new URLSearchParams({ kind, range });
  if (name) q.set("name", name);
  if (group) q.set("group", group);
  const { data } = usePoll(() => api<Resp>(`/servers/${serverId}/usage-history?${q}`), [serverId, kind, name, group, range], 60_000);
  const ts = data?.ts ?? [];
  const sr = data?.series;
  const cpuFmt = kind === "pod" ? (v: number) => (v >= 1000 ? `${(v / 1000).toFixed(2)} cores` : `${Math.round(v)}m`) : (v: number) => `${v < 10 ? v.toFixed(1) : Math.round(v)}%`;
  const last = (a?: (number | null)[]) => [...(a ?? [])].reverse().find((v) => v != null) ?? null;
  const peak = (a?: (number | null)[]) => (a ?? []).reduce<number | null>((m, v) => (v != null && (m == null || v > m) ? v : m), null);
  const limitNote = (r?: number, l?: number, f?: (v: number) => string) =>
    f && (r || l) ? ` · request ${r ? f(r) : "—"} / limit ${l ? f(l) : "—"}` : "";
  return (
    <div className="col" style={{ gap: 12 }}>
      <div className="row wrap">
        <h2 className="grow" style={{ margin: 0 }}>Usage history</h2>
        <Seg
          value={range}
          onChange={setRange}
          options={(["1h", "6h", "24h", "3d", "7d", "30d"] as UsageRange[]).map((r) => ({ value: r, label: r }))}
        />
      </div>
      <div className="grid grid-2">
        <ChartCard title="CPU" accent="cpu" now={sr ? `${cpuFmt(last(sr.cpu) ?? 0)} · peak ${cpuFmt(peak(sr.cpu_max) ?? 0)}` : undefined}>
          <TimeChart
            ts={ts}
            fmt={cpuFmt}
            series={[
              { label: "Average", values: sr?.cpu ?? [], color: "--series-1" },
              { label: "Peak", values: sr?.cpu_max ?? [], color: "--series-2" },
            ]}
            empty="No history yet — samples are stored every 30–60 seconds"
          />
          {kind === "pod" && limits && <div className="small faint" style={{ marginTop: 6 }}>{limitNote(limits.cpu_req, limits.cpu_lim, cpuFmt).slice(3)}</div>}
        </ChartCard>
        <ChartCard title="Memory" accent="mem" now={sr ? `${bytes(last(sr.mem) ?? 0)} · peak ${bytes(peak(sr.mem_max) ?? 0)}` : undefined}>
          <TimeChart
            ts={ts}
            fmt={(v) => bytes(v)}
            series={[
              { label: "Average", values: sr?.mem ?? [], color: "--series-1" },
              { label: "Peak", values: sr?.mem_max ?? [], color: "--series-2" },
            ]}
            empty="No history yet — samples are stored every 30–60 seconds"
          />
          {kind === "pod" && limits && <div className="small faint" style={{ marginTop: 6 }}>{limitNote(limits.mem_req, limits.mem_lim, (v) => bytes(v)).slice(3)}</div>}
        </ChartCard>
      </div>
    </div>
  );
}
