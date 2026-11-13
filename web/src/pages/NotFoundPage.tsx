import { FileText } from "lucide-react";
import { useTranslation } from "react-i18next";

import { hrefOf, ScreenLink } from "../shell/navigation";

/** An address that is not a screen of the app: say so, inside the shell, and offer the archive. */
export function NotFoundPage() {
  const { t } = useTranslation();
  return (
    <div className="stage">
      <section className="not-found-page">
        <FileText aria-hidden="true" />
        <h1>{t("notFound.title")}</h1>
        <p>{t("notFound.description")}</p>
        <ScreenLink className="button button--primary" to={hrefOf("archive")} mode="tab">{t("notFound.toArchive")}</ScreenLink>
      </section>
    </div>
  );
}
