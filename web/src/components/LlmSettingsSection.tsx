import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, PenTool, RotateCcw, Sparkles, Tag, Trash2 } from "lucide-react";
import { useId, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type AdminLlmSettings, type AdminSettingsPatch } from "../api";
import { adminSettingsQuery } from "../queries";
import type { useConfirm } from "./controls/ConfirmDialog";
import { SelectDropdown } from "./controls/SelectDropdown";
import { toast } from "./controls/toast";

type LlmProvider = AdminLlmSettings["provider"];

const REASONING_EFFORTS = ["none", "minimal", "low", "medium", "high", "xhigh"] as const;

function errorCode(cause: unknown): string {
  return cause instanceof ApiError ? cause.code : "network";
}

function parseModels(text: string): string[] {
  return text
    .split(/[\n,]+/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}

function arraysEqual(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

interface LlmDraft {
  enabled: boolean;
  apiKey: string;
  baseUrl: string;
  provider: LlmProvider;
  model: string;
  visionModel: string;
  handwritingEnabled: boolean;
  handwritingModel: string;
  allowedModelsText: string;
  reasoningEffort: string;
  createTags: boolean;
  monthlyBudget: string;
  prices: string;
}

function isDraftDirty(baseline: AdminLlmSettings | null | undefined, draft: LlmDraft): boolean {
  if (!baseline) return false;
  return (
    draft.enabled !== baseline.enabled
    || draft.apiKey.trim().length > 0
    || draft.baseUrl.trim() !== baseline.base_url
    || draft.provider !== baseline.provider
    || draft.model.trim() !== baseline.model
    || draft.visionModel.trim() !== baseline.vision_model
    || draft.handwritingEnabled !== baseline.handwriting_enabled
    || draft.handwritingModel.trim() !== baseline.handwriting_model
    || !arraysEqual(parseModels(draft.allowedModelsText), baseline.allowed_models)
    || (draft.reasoningEffort || null) !== (baseline.reasoning_effort || null)
    || draft.createTags !== baseline.create_tags
    || (draft.monthlyBudget.trim() || null) !== (baseline.monthly_budget_usd || null)
    || draft.prices.trim() !== (baseline.prices ?? "").trim()
  );
}

/**
 * Administrator configuration for LLM document enrichment: API credentials,
 * models, monthly budget, handwriting transcription, and provider options.
 */
export function LlmSettingsSection({
  confirm,
  onError,
}: {
  confirm: ReturnType<typeof useConfirm>["confirm"];
  onError: (code: string | null) => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data } = useQuery(adminSettingsQuery);

  const apiKeyId = useId();
  const apiKeyHintId = useId();
  const modelId = useId();
  const budgetId = useId();
  const handwritingModelId = useId();
  const baseUrlId = useId();
  const visionModelId = useId();
  const allowedModelsId = useId();
  const allowedModelsHintId = useId();
  const pricesId = useId();
  const pricesHintId = useId();

  const [busy, setBusy] = useState(false);
  const [prevData, setPrevData] = useState<AdminLlmSettings | null | undefined>(undefined);

  const [enabled, setEnabled] = useState(true);
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [provider, setProvider] = useState<LlmProvider>("auto");
  const [model, setModel] = useState("");
  const [visionModel, setVisionModel] = useState("");
  const [handwritingEnabled, setHandwritingEnabled] = useState(true);
  const [handwritingModel, setHandwritingModel] = useState("");
  const [allowedModelsText, setAllowedModelsText] = useState("");
  const [reasoningEffort, setReasoningEffort] = useState("");
  const [createTags, setCreateTags] = useState(true);
  const [monthlyBudget, setMonthlyBudget] = useState("");
  const [prices, setPrices] = useState("");

  function applyBaseline(llm: AdminLlmSettings) {
    setEnabled(llm.enabled);
    setBaseUrl(llm.base_url);
    setProvider(llm.provider);
    setModel(llm.model);
    setVisionModel(llm.vision_model);
    setHandwritingEnabled(llm.handwriting_enabled);
    setHandwritingModel(llm.handwriting_model);
    setAllowedModelsText(llm.allowed_models.join("\n"));
    setReasoningEffort(llm.reasoning_effort ?? "");
    setCreateTags(llm.create_tags);
    setMonthlyBudget(llm.monthly_budget_usd ?? "");
    setPrices(llm.prices ?? "");
  }

  const draft: LlmDraft = {
    enabled,
    apiKey,
    baseUrl,
    provider,
    model,
    visionModel,
    handwritingEnabled,
    handwritingModel,
    allowedModelsText,
    reasoningEffort,
    createTags,
    monthlyBudget,
    prices,
  };

  if (data && data.llm !== prevData) {
    const wasDirty = isDraftDirty(prevData, draft);
    setPrevData(data.llm);
    if (!wasDirty) {
      applyBaseline(data.llm);
    }
  }

  if (data === null) return null;

  const llm = data?.llm;
  const dirty = isDraftDirty(llm, draft);

  const statusLabel = llm
    ? !llm.has_api_key
      ? t("settings.llmStatusNotConfigured")
      : llm.enabled
        ? t("settings.llmStatusActive")
        : t("settings.llmStatusPaused")
    : null;
  const statusClass = llm?.has_api_key && llm.enabled ? "settings-badge" : "settings-badge settings-badge--plain";

  const keySourceText = llm
    ? llm.api_key_source === "database"
      ? t("settings.llmKeySourceDatabase")
      : llm.api_key_source === "env"
        ? t("settings.llmKeySourceEnv")
        : t("settings.llmKeySourceNone")
    : "";

  const providerOptions = [
    { value: "auto", label: t("settings.llmProviderAuto") },
    { value: "openrouter", label: t("settings.llmProviderOpenRouter") },
    { value: "generic", label: t("settings.llmProviderGeneric") },
  ];

  const reasoningOptions = [
    { value: "", label: t("settings.llmReasoningDefault") },
    ...REASONING_EFFORTS.map((level) => ({
      value: level,
      label: t(`settings.llmReasoningLevels.${level}`),
    })),
  ];

  async function saveSettings(event: FormEvent) {
    event.preventDefault();
    if (!llm) return;
    setBusy(true);
    onError(null);
    const llmPatch: NonNullable<AdminSettingsPatch["llm"]> = {};
    if (enabled !== llm.enabled) llmPatch.enabled = enabled;
    if (apiKey.trim()) llmPatch.api_key = apiKey.trim();
    if (baseUrl.trim() !== llm.base_url) llmPatch.base_url = baseUrl.trim();
    if (provider !== llm.provider) llmPatch.provider = provider;
    if (model.trim() !== llm.model) llmPatch.model = model.trim();
    if (visionModel.trim() !== llm.vision_model) llmPatch.vision_model = visionModel.trim();
    if (handwritingEnabled !== llm.handwriting_enabled) llmPatch.handwriting_enabled = handwritingEnabled;
    if (handwritingModel.trim() !== llm.handwriting_model) llmPatch.handwriting_model = handwritingModel.trim();
    const parsedAllowed = parseModels(allowedModelsText);
    if (!arraysEqual(parsedAllowed, llm.allowed_models)) llmPatch.allowed_models = parsedAllowed;
    if ((reasoningEffort || null) !== (llm.reasoning_effort || null)) {
      llmPatch.reasoning_effort = reasoningEffort || null;
    }
    if (createTags !== llm.create_tags) llmPatch.create_tags = createTags;
    if ((monthlyBudget.trim() || null) !== (llm.monthly_budget_usd || null)) {
      llmPatch.monthly_budget_usd = monthlyBudget.trim() || null;
    }
    if (prices.trim() !== (llm.prices ?? "").trim()) {
      llmPatch.prices = prices.trim() || null;
    }
    try {
      const updated = await api.updateSettings({ llm: llmPatch });
      queryClient.setQueryData(adminSettingsQuery.queryKey, updated);
      setPrevData(updated.llm);
      applyBaseline(updated.llm);
      setApiKey("");
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["settings-summary"] }),
        queryClient.invalidateQueries({ queryKey: ["stats"] }),
      ]);
      toast(t("settings.llmSavedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function clearSavedKey() {
    setBusy(true);
    onError(null);
    try {
      const updated = await api.updateSettings({ llm: { clear_api_key: true } });
      queryClient.setQueryData(adminSettingsQuery.queryKey, updated);
      setPrevData(updated.llm);
      setApiKey("");
      if (!isDraftDirty(llm, { ...draft, apiKey: "" })) {
        applyBaseline(updated.llm);
      }
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["settings-summary"] }),
        queryClient.invalidateQueries({ queryKey: ["stats"] }),
      ]);
      toast(t("settings.llmKeyClearedToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  async function resetToDefaults() {
    const ok = await confirm({
      title: t("settings.llmResetTitle"),
      description: t("settings.llmResetConfirm"),
      confirmLabel: t("settings.llmResetAction"),
      cancelLabel: t("common.cancel"),
      destructive: true,
    });
    if (!ok) return;

    setBusy(true);
    onError(null);
    try {
      const updated = await api.updateSettings({ reset_llm: true });
      queryClient.setQueryData(adminSettingsQuery.queryKey, updated);
      setPrevData(updated.llm);
      applyBaseline(updated.llm);
      setApiKey("");
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["settings-summary"] }),
        queryClient.invalidateQueries({ queryKey: ["stats"] }),
      ]);
      toast(t("settings.llmResetToast"), { icon: "check" });
    } catch (cause) {
      onError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-section" aria-labelledby="settings-llm">
      <div className="settings-section-heading">
        <div>
          <h3 id="settings-llm">{t("settings.llmTitle")}</h3>
          <p>{t("settings.llmDescription")}</p>
        </div>
        {statusLabel ? <span className={statusClass}>{statusLabel}</span> : null}
      </div>

      {llm ? (
        <form className="settings-form" onSubmit={saveSettings} noValidate>
          <label className="toggle-row">
            <span className="toggle-row-text">
              <Sparkles aria-hidden="true" />
              <span>
                <strong>{t("settings.llmEnabled")}</strong>
                <small>{t("settings.llmEnabledHint")}</small>
              </span>
            </span>
            <input
              type="checkbox"
              role="switch"
              className="switch"
              checked={enabled}
              onChange={(event) => setEnabled(event.target.checked)}
              aria-label={t("settings.llmEnabled")}
            />
          </label>

          <div className="settings-key-block">
            <label className="field-label" htmlFor={apiKeyId}>
              <span>{t("settings.llmApiKey")}</span>
              <div className="settings-key-row">
                <input
                  id={apiKeyId}
                  type="password"
                  autoComplete="new-password"
                  value={apiKey}
                  onChange={(event) => setApiKey(event.target.value)}
                  placeholder={llm.has_api_key ? t("settings.llmApiKeyConfiguredPlaceholder") : t("settings.llmApiKeyPlaceholder")}
                  aria-label={t("settings.llmApiKey")}
                  aria-describedby={apiKeyHintId}
                />
                {llm.api_key_source === "database" ? (
                  <button
                    type="button"
                    className="button button--secondary"
                    disabled={busy}
                    onClick={() => void clearSavedKey()}
                  >
                    <Trash2 aria-hidden="true" />{t("settings.llmClearKey")}
                  </button>
                ) : null}
              </div>
            </label>
            <p id={apiKeyHintId} className="email-field-hint">{keySourceText}</p>
          </div>

          <div className="settings-form-grid">
            <label className="field-label" htmlFor={modelId}>
              <span>{t("settings.llmModel")}</span>
              <input
                id={modelId}
                type="text"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="none"
                spellCheck={false}
                value={model}
                onChange={(event) => setModel(event.target.value)}
                aria-label={t("settings.llmModel")}
              />
            </label>
            <label className="field-label" htmlFor={budgetId}>
              <span>{t("settings.llmBudget")}</span>
              <input
                id={budgetId}
                type="text"
                inputMode="decimal"
                autoComplete="off"
                value={monthlyBudget}
                placeholder={t("settings.llmBudgetUnlimited")}
                onChange={(event) => setMonthlyBudget(event.target.value)}
                aria-label={t("settings.llmBudget")}
              />
            </label>
          </div>

          <label className="toggle-row">
            <span className="toggle-row-text">
              <PenTool aria-hidden="true" />
              <span>
                <strong>{t("settings.llmHandwritingEnabled")}</strong>
                <small>{t("settings.llmHandwritingHint")}</small>
              </span>
            </span>
            <input
              type="checkbox"
              role="switch"
              className="switch"
              checked={handwritingEnabled}
              onChange={(event) => setHandwritingEnabled(event.target.checked)}
              aria-label={t("settings.llmHandwritingEnabled")}
            />
          </label>

          {/* The model only matters while handwriting is re-read; hidden otherwise, kept in the draft. */}
          {handwritingEnabled ? (
            <label className="field-label settings-dependent-field" htmlFor={handwritingModelId}>
              <span>{t("settings.llmHandwritingModel")}</span>
              <input
                id={handwritingModelId}
                type="text"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="none"
                spellCheck={false}
                value={handwritingModel}
                onChange={(event) => setHandwritingModel(event.target.value)}
                aria-label={t("settings.llmHandwritingModel")}
              />
            </label>
          ) : null}

          <label className="toggle-row">
            <span className="toggle-row-text">
              <Tag aria-hidden="true" />
              <span>
                <strong>{t("settings.llmCreateTags")}</strong>
                <small>{t("settings.llmCreateTagsHint")}</small>
              </span>
            </span>
            <input
              type="checkbox"
              role="switch"
              className="switch"
              checked={createTags}
              onChange={(event) => setCreateTags(event.target.checked)}
              aria-label={t("settings.llmCreateTags")}
            />
          </label>

          <details className="processing-details settings-advanced-details">
            <summary>
              <span>{t("settings.llmAdvanced")}</span>
              <span>{t("settings.llmAdvancedAction")}<ChevronDown className="disclosure-chevron" aria-hidden="true" /></span>
            </summary>
            <div className="settings-advanced-body">
              <div className="settings-form-grid">
                <div className="field-label">
                  <span>{t("settings.llmProvider")}</span>
                  <SelectDropdown
                    value={provider}
                    onChange={(value) => setProvider(value as LlmProvider)}
                    options={providerOptions}
                    aria-label={t("settings.llmProvider")}
                  />
                </div>
                <label className="field-label" htmlFor={baseUrlId}>
                  <span>{t("settings.llmBaseUrl")}</span>
                  <input
                    id={baseUrlId}
                    type="text"
                    inputMode="url"
                    autoComplete="off"
                    autoCorrect="off"
                    autoCapitalize="none"
                    spellCheck={false}
                    value={baseUrl}
                    onChange={(event) => setBaseUrl(event.target.value)}
                    aria-label={t("settings.llmBaseUrl")}
                  />
                </label>
                <label className="field-label" htmlFor={visionModelId}>
                  <span>{t("settings.llmVisionModel")}</span>
                  <input
                    id={visionModelId}
                    type="text"
                    autoComplete="off"
                    autoCorrect="off"
                    autoCapitalize="none"
                    spellCheck={false}
                    value={visionModel}
                    onChange={(event) => setVisionModel(event.target.value)}
                    aria-label={t("settings.llmVisionModel")}
                  />
                </label>
                <div className="field-label">
                  <span>{t("settings.llmReasoningEffort")}</span>
                  <SelectDropdown
                    value={reasoningEffort}
                    onChange={setReasoningEffort}
                    options={reasoningOptions}
                    aria-label={t("settings.llmReasoningEffort")}
                  />
                </div>
              </div>

              <label className="field-label" htmlFor={allowedModelsId}>
                <span>{t("settings.llmAllowedModels")}</span>
                <textarea
                  id={allowedModelsId}
                  rows={3}
                  value={allowedModelsText}
                  onChange={(event) => setAllowedModelsText(event.target.value)}
                  placeholder={t("settings.llmAllowedModelsPlaceholder")}
                  aria-label={t("settings.llmAllowedModels")}
                  aria-describedby={allowedModelsHintId}
                  autoCorrect="off"
                  autoCapitalize="none"
                  spellCheck={false}
                />
              </label>
              <p id={allowedModelsHintId} className="email-field-hint">{t("settings.llmAllowedModelsHint")}</p>

              <label className="field-label" htmlFor={pricesId}>
                <span>{t("settings.llmPrices")}</span>
                <input
                  id={pricesId}
                  type="text"
                  autoComplete="off"
                  autoCorrect="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  value={prices}
                  placeholder="model=input:output"
                  onChange={(event) => setPrices(event.target.value)}
                  aria-label={t("settings.llmPrices")}
                  aria-describedby={pricesHintId}
                />
              </label>
              <p id={pricesHintId} className="email-field-hint">{t("settings.llmPricesHint")}</p>

              {llm.has_overrides ? (
                <div className="settings-reset-row">
                  <button
                    type="button"
                    className="quiet-button settings-action-danger"
                    disabled={busy}
                    onClick={() => void resetToDefaults()}
                  >
                    <RotateCcw aria-hidden="true" />{t("settings.llmResetButton")}
                  </button>
                </div>
              ) : null}
            </div>
          </details>

          <div className="settings-form-footer">
            <div>
              {dirty ? <span className="taxonomy-dirty" role="status">{t("taxonomy.unsaved")}</span> : null}
            </div>
            <div className="settings-form-actions">
              <button type="submit" className="button button--primary" disabled={busy || !dirty}>
                {t("settings.llmSave")}
              </button>
            </div>
          </div>
        </form>
      ) : null}
    </section>
  );
}
