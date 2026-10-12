"use client";

import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { Save } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Controller, useForm } from "react-hook-form";
import { aiEfforts, aiModelName, aiSettingsFromForm, aiSettingsToForm } from "../../lib/ai";
import { errorMessage } from "../../lib/api/client";
import type { components } from "../../lib/api/generated";
import { browserApi } from "../../lib/api/operations";
import { formatUsd, usdInputValue } from "../../lib/format";
import { queryKeys } from "../../lib/query";
import { aiSettingsFormSchema, type AiSettingsFormValues } from "../../lib/schemas";
import { Button, controlStyles, Field, Notice, Surface, SwitchField } from "../ui";

type AiDefaults = components["schemas"]["AiDefaults"];
type AiModel = components["schemas"]["AiModel"];
type AiSettings = components["schemas"]["AiSettings"];

export function AiSettingsForm({
  serverId,
  settings,
  models,
  defaults,
}: {
  serverId: string;
  settings: AiSettings;
  models: AiModel[];
  defaults: AiDefaults;
}) {
  const queryClient = useQueryClient();
  const accountBudget = defaults.account_monthly_budget_micros;
  // The control plane caps the default server budget at the account budget.
  const defaultBudget = Math.min(defaults.server_monthly_budget_micros, accountBudget);
  const schema = useMemo(() => aiSettingsFormSchema(accountBudget), [accountBudget]);
  const saved = useMemo(() => aiSettingsToForm(settings, models), [settings, models]);
  const {
    register,
    control,
    reset,
    handleSubmit,
    formState: { errors, isDirty, isSubmitting },
  } = useForm<AiSettingsFormValues>({ resolver: zodResolver(schema), defaultValues: saved });
  const savedKey = useRef(JSON.stringify(saved));
  const [requestError, setRequestError] = useState<string | null>(null);

  // Live updates refresh a clean form but never overwrite a draft.
  useEffect(() => {
    const nextKey = JSON.stringify(saved);
    if (nextKey !== savedKey.current && !isDirty) {
      savedKey.current = nextKey;
      reset(saved);
    }
  }, [saved, isDirty, reset]);

  const save = handleSubmit(async (values) => {
    setRequestError(null);
    try {
      const updated = await browserApi.updateAiSettings(serverId, aiSettingsFromForm(values));
      queryClient.setQueryData(queryKeys.ai(serverId), updated);
      const next = aiSettingsToForm(updated.settings, updated.models);
      savedKey.current = JSON.stringify(next);
      reset(next);
    } catch (error) {
      setRequestError(errorMessage(error));
    }
  });

  return (
    <Surface className="p-6">
      <h2 className="m-0 font-display text-xl leading-[1.15] font-[800]">Settings</h2>
      <p className="mt-1.5 mb-0 text-xs leading-[1.45] text-ink-soft">
        These apply to every operator&apos;s requests on this server.
      </p>
      <form className="mt-4 grid gap-4" onSubmit={save} noValidate>
        <Controller
          control={control}
          name="enabled"
          render={({ field }) => (
            <SwitchField
              label="Enabled"
              description="Operators can use /ai on this server."
              checked={field.value}
              onCheckedChange={field.onChange}
            />
          )}
        />
        <Field label="Model" help="Prices are per million input / output tokens.">
          <select className={controlStyles.select} aria-label="Model" {...register("model")}>
            <option value="">Default ({aiModelName(models, defaults.model)})</option>
            {models.map((model) => (
              <option key={model.id} value={model.id}>
                {model.name} · {formatUsd(model.input_micros_per_mtok)} / {formatUsd(model.output_micros_per_mtok)}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Effort" help="Higher effort thinks longer before acting and costs more.">
          <select className={controlStyles.select} aria-label="Effort" {...register("effort")}>
            <option value="">Default ({aiEfforts[defaults.effort]})</option>
            {Object.entries(aiEfforts).map(([effort, label]) => (
              <option key={effort} value={effort}>{label}</option>
            ))}
          </select>
        </Field>
        <Field
          label="Monthly budget in US dollars"
          error={errors.monthly_budget?.message}
          help={`Leave empty for the default of ${formatUsd(defaultBudget)}. The account allows ${formatUsd(accountBudget)} a month across all of its servers.`}
        >
          <input
            className={controlStyles.input}
            aria-label="Monthly budget in US dollars"
            inputMode="decimal"
            autoComplete="off"
            placeholder={usdInputValue(defaultBudget)}
            {...register("monthly_budget")}
          />
        </Field>
        {requestError ? <Notice tone="danger">{requestError}</Notice> : null}
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-[#ddd4c3] pt-4">
          {isDirty ? (
            <span className="flex items-center gap-2 text-xs font-[720] text-[#7c5a25]">
              <i className="size-2 rounded-[3px] bg-warning" />
              Unsaved draft
            </span>
          ) : (
            <span className="text-xs text-ink-soft">Everything is saved.</span>
          )}
          <Button type="submit" disabled={!isDirty || isSubmitting}>
            {isSubmitting ? "Saving…" : <><Save size={16} />Save settings</>}
          </Button>
        </div>
      </form>
    </Surface>
  );
}
