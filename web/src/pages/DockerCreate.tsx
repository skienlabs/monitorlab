import { useState } from "react";
import { Download, Plus } from "lucide-react";
import { rpc } from "../api";
import { Dialog, Spinner, useToast } from "../components/ui";

const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);

/** Shared shell: a dialog with one submit button that shows errors under the form. */
function Form({ title, icon, onClose, submit, label, busy, error, children, wide }: { title: string; icon: React.ReactNode; onClose: () => void; submit: () => void; label: string; busy: boolean; error: string | null; children: React.ReactNode; wide?: boolean }) {
  return (
    <Dialog
      wide={wide}
      title={title}
      icon={icon}
      onClose={onClose}
      footer={
        <>
          <button className="btn text" onClick={onClose}>Cancel</button>
          <button className="btn filled" disabled={busy} onClick={submit}>{busy && <Spinner />} {label}</button>
        </>
      }
    >
      {children}
      {error && <div className="banner crit" style={{ marginTop: 12 }}>{error}</div>}
    </Dialog>
  );
}

function useRun(serverId: string, onDone: () => void, onClose: () => void) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (req: Record<string, unknown>, ok: string) => {
    setBusy(true);
    setError(null);
    try {
      await rpc(serverId, req);
      toast(ok);
      onDone();
      onClose();
    } catch (e: any) {
      setError(e.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };
  return { busy, error, run };
}

export function PullImageDialog({ serverId, onClose, onDone }: { serverId: string; onClose: () => void; onDone: () => void }) {
  const [image, setImage] = useState("");
  const { busy, error, run } = useRun(serverId, onDone, onClose);
  const go = () => image.trim() && run({ op: "docker_image_pull", image: image.trim() }, `Pulled ${image.trim()}`);
  return (
    <Form title="Pull an image" icon={<Download size={20} />} onClose={onClose} submit={go} label={busy ? "Pulling…" : "Pull"} busy={busy} error={error}>
      <div className="field">
        <label>Image</label>
        <input className="input" autoFocus placeholder="e.g. nginx:1.27 or ghcr.io/org/app:latest" value={image} onChange={(e) => setImage(e.target.value)} onKeyDown={(e) => e.key === "Enter" && go()} />
        <span className="hint">Private registries use the credentials from `docker login` on that server. Large images can take a few minutes.</span>
      </div>
    </Form>
  );
}

export function NewVolumeDialog({ serverId, onClose, onDone }: { serverId: string; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState("");
  const { busy, error, run } = useRun(serverId, onDone, onClose);
  const go = () => name.trim() && run({ op: "docker_volume_create", name: name.trim() }, `Created volume ${name.trim()}`);
  return (
    <Form title="New volume" icon={<Plus size={20} />} onClose={onClose} submit={go} label="Create" busy={busy} error={error}>
      <div className="field">
        <label>Name</label>
        <input className="input" autoFocus placeholder="e.g. app-data" value={name} maxLength={128} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && go()} />
      </div>
    </Form>
  );
}

export function NewNetworkDialog({ serverId, onClose, onDone }: { serverId: string; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState("");
  const [subnet, setSubnet] = useState("");
  const { busy, error, run } = useRun(serverId, onDone, onClose);
  const go = () => name.trim() && run({ op: "docker_network_create", name: name.trim(), subnet: subnet.trim() || undefined }, `Created network ${name.trim()}`);
  return (
    <Form title="New network" icon={<Plus size={20} />} onClose={onClose} submit={go} label="Create" busy={busy} error={error}>
      <div className="field">
        <label>Name</label>
        <input className="input" autoFocus placeholder="e.g. backend" value={name} maxLength={128} onChange={(e) => setName(e.target.value)} />
      </div>
      <div className="field">
        <label>Subnet (optional)</label>
        <input className="input" placeholder="e.g. 172.30.0.0/24" value={subnet} onChange={(e) => setSubnet(e.target.value)} onKeyDown={(e) => e.key === "Enter" && go()} />
      </div>
    </Form>
  );
}

export function NewContainerDialog({ serverId, image: initialImage, onClose, onDone }: { serverId: string; image?: string; onClose: () => void; onDone: () => void }) {
  const [image, setImage] = useState(initialImage ?? "");
  const [name, setName] = useState("");
  const [ports, setPorts] = useState("");
  const [env, setEnv] = useState("");
  const [volumes, setVolumes] = useState("");
  const [restart, setRestart] = useState("unless-stopped");
  const [network, setNetwork] = useState("");
  const [cmd, setCmd] = useState("");
  const [priv, setPriv] = useState(false);
  const [pull, setPull] = useState(true);
  const [start, setStart] = useState(true);
  const { busy, error, run } = useRun(serverId, onDone, onClose);
  const go = () =>
    image.trim() &&
    run(
      {
        op: "docker_container_create",
        image: image.trim(),
        name: name.trim() || undefined,
        ports: lines(ports),
        env: lines(env),
        volumes: lines(volumes),
        restart,
        network: network.trim() || undefined,
        cmd: cmd.trim() ? cmd.trim().split(/\s+/) : undefined,
        privileged: priv,
        pull,
        start,
      },
      start ? `Started a container from ${image.trim()}` : `Created a container from ${image.trim()}`,
    );
  return (
    <Form wide title="Run a new container" icon={<Plus size={20} />} onClose={onClose} submit={go} label={busy ? (pull ? "Pulling and starting…" : "Starting…") : start ? "Run" : "Create"} busy={busy} error={error}>
      <div className="row" style={{ gap: 12, alignItems: "flex-start", flexWrap: "wrap" }}>
        <div className="field grow" style={{ minWidth: 240 }}>
          <label>Image</label>
          <input className="input" autoFocus placeholder="e.g. nginx:1.27" value={image} onChange={(e) => setImage(e.target.value)} />
        </div>
        <div className="field grow" style={{ minWidth: 200 }}>
          <label>Name (optional)</label>
          <input className="input" placeholder="e.g. web" value={name} onChange={(e) => setName(e.target.value)} />
        </div>
      </div>
      <div className="row" style={{ gap: 12, alignItems: "flex-start", flexWrap: "wrap" }}>
        <div className="field grow" style={{ minWidth: 240 }}>
          <label>Published ports (one per line)</label>
          <textarea className="input" rows={3} placeholder={"8080:80\n127.0.0.1:5432:5432\n53:53/udp"} value={ports} onChange={(e) => setPorts(e.target.value)} />
        </div>
        <div className="field grow" style={{ minWidth: 240 }}>
          <label>Environment (one per line)</label>
          <textarea className="input" rows={3} placeholder={"TZ=Asia/Kolkata\nAPP_MODE=prod"} value={env} onChange={(e) => setEnv(e.target.value)} />
        </div>
      </div>
      <div className="field">
        <label>Volumes (one per line)</label>
        <textarea className="input" rows={3} placeholder={"app-data:/var/lib/app\n/srv/files:/files:ro"} value={volumes} onChange={(e) => setVolumes(e.target.value)} />
        <span className="hint">A volume name creates a Docker volume; a path starting with / mounts that folder from the server.</span>
      </div>
      <div className="row" style={{ gap: 12, alignItems: "flex-start", flexWrap: "wrap" }}>
        <div className="field" style={{ minWidth: 180 }}>
          <label>Restart policy</label>
          <select className="select" value={restart} onChange={(e) => setRestart(e.target.value)}>
            <option value="no">Never</option>
            <option value="unless-stopped">Unless stopped</option>
            <option value="always">Always</option>
            <option value="on-failure">On failure</option>
          </select>
        </div>
        <div className="field grow" style={{ minWidth: 200 }}>
          <label>Network (optional)</label>
          <input className="input" placeholder="default bridge" value={network} onChange={(e) => setNetwork(e.target.value)} />
        </div>
        <div className="field grow" style={{ minWidth: 200 }}>
          <label>Command (optional)</label>
          <input className="input" placeholder="image default" value={cmd} onChange={(e) => setCmd(e.target.value)} />
        </div>
      </div>
      <div className="col" style={{ gap: 6 }}>
        <label className="check"><input type="checkbox" checked={pull} onChange={(e) => setPull(e.target.checked)} /> Pull the latest image first</label>
        <label className="check"><input type="checkbox" checked={start} onChange={(e) => setStart(e.target.checked)} /> Start it right away</label>
        <label className="check"><input type="checkbox" checked={priv} onChange={(e) => setPriv(e.target.checked)} /> Privileged (full access to the host, avoid unless you know you need it)</label>
      </div>
    </Form>
  );
}
