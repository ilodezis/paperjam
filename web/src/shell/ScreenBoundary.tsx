import { AlertCircle, RefreshCw } from "lucide-react";
import { Component, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { QueryErrorResetBoundary } from "@tanstack/react-query";

import { SCREEN_TITLE_KEY, type ScreenId } from "./nav";
import { TopBar } from "./PageHead";

export interface CatchProps {
  /** Lets the failed queries run again before the screen is drawn anew. */
  onReset: () => void;
  fallback: (retry: () => void) => ReactNode;
  children: ReactNode;
}

export class Catch extends Component<CatchProps, { failed: boolean }> {
  override state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  retry = () => {
    this.props.onReset();
    this.setState({ failed: false });
  };

  override render() {
    return this.state.failed ? this.props.fallback(this.retry) : this.props.children;
  }
}

/** What a screen shows when it could not be drawn: its own name, one line saying so and one action. */
export function ScreenFailed({ id, onRetry }: { id: ScreenId; onRetry: () => void }) {
  const { t } = useTranslation();
  const title = t(SCREEN_TITLE_KEY[id]);
  return (
    <>
      <TopBar title={title} />
      <div className="document-unavailable" role="alert">
        <AlertCircle aria-hidden="true" />
        <h1>{title}</h1>
        <p>{t("app.loadFailed")}<small>{t("app.loadFailedSub")}</small></p>
        <button type="button" className="button button--secondary" onClick={onRetry}><RefreshCw aria-hidden="true" />{t("common.retry")}</button>
      </div>
    </>
  );
}

/** Keeps a failure inside the screen it happened on: the shell, the navigation and the other
 *  screens of the stack stay as they are. */
export function ScreenBoundary({ id, children }: { id: ScreenId; children: ReactNode }) {
  return (
    <QueryErrorResetBoundary>
      {({ reset }) => (
        <Catch onReset={reset} fallback={(retry) => <ScreenFailed id={id} onRetry={retry} />}>
          {children}
        </Catch>
      )}
    </QueryErrorResetBoundary>
  );
}
