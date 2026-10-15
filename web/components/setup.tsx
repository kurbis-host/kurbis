"use client";

import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowRight } from "lucide-react";
import { useRouter } from "next/navigation";
import { Controller, useForm } from "react-hook-form";
import { errorMessage } from "../lib/api/client";
import { browserApi } from "../lib/api/operations";
import { queryKeys } from "../lib/query";
import { setupSchema, type SetupValues } from "../lib/schemas";
import {
  Button,
  controlStyles,
  Field,
  Notice,
  PageHeader,
  Surface,
  SwitchField,
} from "./ui";

const defaults: SetupValues = {
  subdomain: "",
  preset: "survival",
  motd: "Hosted by Kurbis",
  difficulty: "Normal",
  gamemode: "Survival",
  view_distance: 12,
  simulation_distance: 8,
  hardcore: false,
  whitelist: true,
  whitelist_by_members: true,
  tab_stats: false,
};

const presetCard =
  "block min-h-[126px] rounded-xl bg-[#eee6d5] p-[18px] shadow-[inset_0_0_0_1px_#c9c0ad] transition-[transform,box-shadow,background-color] duration-150 peer-checked:bg-[#dfe8d0] peer-checked:shadow-[inset_0_0_0_2px_var(--color-grass),0_8px_18px_rgba(78,99,54,0.10)] peer-focus-visible:outline-3 peer-focus-visible:outline-offset-3 peer-focus-visible:outline-focus";

export function Setup() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const form = useForm<SetupValues>({
    resolver: zodResolver(setupSchema),
    defaultValues: defaults,
  });
  const {
    register,
    handleSubmit,
    control,
    setValue,
    watch,
    formState: { errors, isSubmitting },
  } = form;
  const preset = watch("preset");

  const submit = handleSubmit(async (values) => {
    try {
      const settings = {
        motd: values.motd,
        difficulty: values.difficulty,
        gamemode: values.gamemode,
        view_distance: values.view_distance,
        simulation_distance: values.simulation_distance,
        hardcore: values.hardcore,
        whitelist: values.whitelist,
        whitelist_by_members: values.whitelist_by_members,
        tab_stats: values.tab_stats,
      };
      const server = await browserApi.createServer({ subdomain: values.subdomain, settings });
      queryClient.setQueryData(queryKeys.server(server.id), server);
      queryClient.setQueryData(queryKeys.servers, [server]);
      router.replace(`/manage/${server.id}`);
    } catch (error) {
      form.setError("root", { message: errorMessage(error) });
    }
  });

  function choosePreset(next: "survival" | "creative") {
    setValue("preset", next, { shouldDirty: true });
    if (next === "survival") {
      setValue("gamemode", "Survival", { shouldDirty: true });
      setValue("difficulty", "Normal", { shouldDirty: true });
      setValue("hardcore", false, { shouldDirty: true });
      return;
    }
    setValue("gamemode", "Creative", { shouldDirty: true });
    setValue("difficulty", "Peaceful", { shouldDirty: true });
    setValue("hardcore", false, { shouldDirty: true });
  }

  return (
    <>
      <PageHeader
        title="Plant your world"
        description="Choose the address and a few core rules. The server is created asleep, ready to wake from the panel or the first player connection."
      />
      {form.formState.errors.root?.message ? (
        <div className="mb-[18px]">
          <Notice tone="danger">{form.formState.errors.root.message}</Notice>
        </div>
      ) : null}

      <div className="grid grid-cols-[minmax(0,1.2fr)_minmax(290px,0.8fr)] items-start gap-5 max-[930px]:grid-cols-1">
        <Surface className="p-[clamp(22px,4vw,34px)]">
          <form
            className="grid grid-cols-2 gap-[18px] max-sm:grid-cols-1"
            onSubmit={submit}
            noValidate
          >
            <div className="col-[1/-1] max-sm:col-auto">
              <Field
                label="World address"
                error={errors.subdomain?.message}
                help="3–32 lowercase letters, digits, or dashes."
              >
                <div className="grid grid-cols-[minmax(0,1fr)_auto] items-stretch">
                  <input
                    className={`${controlStyles.input} rounded-r-none`}
                    autoComplete="off"
                    placeholder="friends-world"
                    {...register("subdomain", {
                      onChange: (event) => {
                        event.target.value = event.target.value
                          .toLowerCase()
                          .replace(/[^a-z0-9-]/g, "");
                      },
                    })}
                  />
                  <span className="flex min-h-11 items-center rounded-r-sm border border-l-0 border-[#bdb5a4] bg-[#e6dfd0] px-3 text-xs text-ink-soft max-sm:max-w-[132px] max-sm:overflow-hidden max-sm:text-ellipsis">
                    .your-domain
                  </span>
                </div>
              </Field>
            </div>

            <div className="col-[1/-1] max-sm:col-auto">
              <Field label="Starting preset">
                <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
                  <label className="relative block cursor-pointer">
                    <input
                      className="peer pointer-events-none absolute opacity-0"
                      type="radio"
                      value="survival"
                      checked={preset === "survival"}
                      onChange={() => choosePreset("survival")}
                    />
                    <span className={presetCard}>
                      <strong className="block font-display text-[19px] leading-[1.1] font-[800]">
                        Survival
                      </strong>
                      <span className="mt-2 block text-[13px] leading-[1.45] text-ink-soft">
                        Normal difficulty, standard progression, and balanced
                        distances for a shared world.
                      </span>
                    </span>
                  </label>
                  <label className="relative block cursor-pointer">
                    <input
                      className="peer pointer-events-none absolute opacity-0"
                      type="radio"
                      value="creative"
                      checked={preset === "creative"}
                      onChange={() => choosePreset("creative")}
                    />
                    <span className={presetCard}>
                      <strong className="block font-display text-[19px] leading-[1.1] font-[800]">
                        Creative
                      </strong>
                      <span className="mt-2 block text-[13px] leading-[1.45] text-ink-soft">
                        Peaceful by default, built for construction sessions and
                        experimentation.
                      </span>
                    </span>
                  </label>
                </div>
              </Field>
            </div>

            <Field label="MOTD" error={errors.motd?.message}>
              <input className={controlStyles.input} {...register("motd")} />
            </Field>
            <Field label="Difficulty" error={errors.difficulty?.message}>
              <select className={controlStyles.select} {...register("difficulty")}>
                <option>Peaceful</option>
                <option>Easy</option>
                <option>Normal</option>
                <option>Hard</option>
              </select>
            </Field>
            <Field label="Gamemode" error={errors.gamemode?.message}>
              <select className={controlStyles.select} {...register("gamemode")}>
                <option>Survival</option>
                <option>Creative</option>
                <option>Adventure</option>
                <option>Spectator</option>
              </select>
            </Field>
            <Field label="View distance" error={errors.view_distance?.message}>
              <input
                className={controlStyles.input}
                type="number"
                min={2}
                max={32}
                {...register("view_distance", { valueAsNumber: true })}
              />
            </Field>
            <Field
              label="Simulation distance"
              error={errors.simulation_distance?.message}
            >
              <input
                className={controlStyles.input}
                type="number"
                min={2}
                max={32}
                {...register("simulation_distance", { valueAsNumber: true })}
              />
            </Field>

            <div className="col-[1/-1] grid grid-cols-2 gap-x-7 gap-y-3 py-1 max-sm:col-auto max-sm:grid-cols-1">
              <Controller
                control={control}
                name="hardcore"
                render={({ field }) => (
                  <SwitchField
                    label="Hardcore"
                    description="Single-life world rules."
                    checked={field.value}
                    onCheckedChange={field.onChange}
                  />
                )}
              />
              <Controller
                control={control}
                name="whitelist"
                render={({ field }) => (
                  <SwitchField
                    label="Whitelist"
                    description="Only explicitly allowed players may join."
                    checked={field.value}
                    onCheckedChange={field.onChange}
                  />
                )}
              />
              <Controller
                control={control}
                name="whitelist_by_members"
                render={({ field }) => (
                  <SwitchField
                    label="Members can let players in"
                    description="Players on the whitelist can add someone it turned away, from in game. Operators always can."
                    checked={field.value}
                    onCheckedChange={field.onChange}
                  />
                )}
              />
            </div>

            <div className="col-[1/-1] mt-2 flex items-center justify-between gap-4 border-t border-[#ddd4c3] pt-5 max-sm:col-auto max-sm:flex-col max-sm:items-stretch max-sm:[&>button]:w-full">
              <p className="m-0 max-w-[46ch] text-xs leading-[1.45] text-ink-soft">
                Creation does not start a worker. Your first snapshot lineage
                begins when the world is run and saved.
              </p>
              <Button type="submit" disabled={isSubmitting}>
                {isSubmitting ? (
                  "Creating world…"
                ) : (
                  <>
                    Create sleeping world <ArrowRight size={17} />
                  </>
                )}
              </Button>
            </div>
          </form>
        </Surface>

        <aside className="sticky top-8 grid gap-4 max-[930px]:static max-[930px]:grid-cols-2 max-sm:grid-cols-1">
          <section className="rounded-md bg-[#302b25] p-6 text-[#f8f0df] shadow-low">
            <h2 className="m-0 font-display text-[22px] leading-[1.15] font-[800]">
              What happens next
            </h2>
            <ol className="mt-5 mb-0 grid list-none gap-[17px] p-0">
              {[
                "The world record is created asleep, using no worker capacity.",
                "Copy the address for friends or press Start from the overview.",
                "When everyone leaves, a five-minute idle timer begins.",
                "Kurbis saves a clean snapshot before releasing the worker.",
              ].map((text, index) => (
                <li
                  className="grid grid-cols-[34px_1fr] items-start gap-[11px] text-[13px] leading-[1.45] text-[#cec5b5]"
                  key={text}
                >
                  <span className="grid size-8 place-items-center rounded-sm bg-[#4c443a] font-[800] text-[#f1a25f]">
                    {index + 1}
                  </span>
                  <span>{text}</span>
                </li>
              ))}
            </ol>
          </section>

          <Surface className="grid grid-cols-2 gap-3.5 p-[22px]">
            {[
              ["2 GiB", "RAM"],
              ["5 GiB", "storage"],
              ["Java", "supported"],
              ["Bedrock", "supported"],
            ].map(([value, label]) => (
              <div className="rounded-[9px] bg-[#eee7d8] p-3.5" key={value}>
                <strong className="block font-display text-xl leading-none font-[800]">
                  {value}
                </strong>
                <span className="mt-1.5 block text-[11px] text-ink-soft">
                  {label}
                </span>
              </div>
            ))}
          </Surface>
        </aside>
      </div>
    </>
  );
}
