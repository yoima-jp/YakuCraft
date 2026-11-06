import { useEffect, useState } from "react";

import { Select } from "../../components/Controls";
import { useT } from "../../i18n";
import { detectMinecraftJars } from "../../lib/tauri";
import type { MinecraftJarCandidate } from "../../types/preview";
import { changeMinecraftJar, loadMinecraftJarState, useMinecraftJarStore } from "../preview/minecraftJarStore";

export function MinecraftJarSection() {
  const translate = useT();
  const [candidates, setCandidates] = useState<MinecraftJarCandidate[]>([]);
  const [selectedPath, setSelectedPath] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState(false);

  useEffect(() => {
    let active = true;
    void Promise.all([detectMinecraftJars(), loadMinecraftJarState()])
      .then(([detected]) => {
        if (!active) return;
        const { path: selected, error } = useMinecraftJarStore.getState();
        if (error) throw new Error("Minecraft JAR setting unavailable");
        setCandidates(detected);
        setSelectedPath(selected ?? "");
      })
      .catch(() => {
        if (active) setError(true);
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => { active = false; };
  }, []);

  const onChange = async (path: string) => {
    setSaving(true);
    setError(false);
    try {
      await changeMinecraftJar(path || null);
      setSelectedPath(path);
    } catch {
      setError(true);
    } finally {
      setSaving(false);
    }
  };

  return (
    <section aria-labelledby="minecraft-jar-title">
      <h2 id="minecraft-jar-title">{translate("settings.minecraftJar")}</h2>
      <div className="settings-field">
        <span>{translate("settings.minecraftVersion")}</span>
        <Select
          aria-label={translate("settings.minecraftVersionAria")}
          value={selectedPath}
          disabled={loading || saving}
          onChange={(path) => { void onChange(path); }}
          options={[
            { value: "", label: translate("settings.notSpecified") },
            ...(selectedPath && !candidates.some((candidate) => candidate.path === selectedPath)
              ? [{ value: selectedPath, label: selectedPath }]
              : []),
            ...candidates.map((candidate) => ({ value: candidate.path, label: candidate.version })),
          ]}
        />
      </div>
      {/* Minecraft本体の素材は再配布せず、指定JARからプレビュー時に読むため、この事情はUIではなく実装コメントに残す。 */}
      <p>{translate("settings.minecraftJarHelp")}</p>
      {error && <p role="alert">{translate("settings.minecraftJarError")}</p>}
    </section>
  );
}
