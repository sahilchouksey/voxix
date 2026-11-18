import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Group, Row, Btn, Screen } from "./ui";
import { InfoIcon } from "./icons";
import { VxLogo } from "./VxLogo";
import { useTranslation } from "react-i18next";

export const AboutTab = () => {
  const { t } = useTranslation();
  const [version, setVersion] = useState("");

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch((e) => console.error("Failed to get app version:", e));
  }, []);

  return (
    <Screen
      center
      title="voxix"
      caption={`Local speech-to-text · version ${version}`}
    >
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 8,
        }}
      >
        <VxLogo size={48} />
      </div>
      <Group title="Version">
        <Row
          icon={<InfoIcon size={16} />}
          title="Version"
          control={<span className="voxix-cap">{version}</span>}
        />
      </Group>
      <Group title="Links">
        <Row
          title="voxix is open source"
          control={
            <Btn
              onClick={() => openUrl("https://github.com/sahilchouksey/voxix")}
            >
              {t("voxix.about.visitGithub")}
            </Btn>
          }
        />
      </Group>
      <div className="voxix-cap" style={{ textAlign: "center" }}>
        {t("voxix.about.footer")}
      </div>
    </Screen>
  );
};
