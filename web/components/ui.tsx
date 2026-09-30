"use client";

import * as Dialog from "@radix-ui/react-dialog";
import * as Switch from "@radix-ui/react-switch";
import { AlertCircle, Box, Info, TriangleAlert } from "lucide-react";
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { useState } from "react";

const buttonBase =
  "inline-flex min-h-11 cursor-pointer items-center justify-center gap-[9px] rounded-sm border-0 px-4 font-[760] transition-[transform,box-shadow,background-color] duration-150 ease-out active:enabled:translate-y-0.5 disabled:cursor-not-allowed disabled:opacity-45";

const buttonVariants = {
  primary:
    "bg-pumpkin text-[#25170f] shadow-[0_5px_0_var(--color-pumpkin-dark),var(--shadow-low)] enabled:hover:bg-[#eb7a2d]",
  secondary:
    "bg-paper text-ink shadow-[inset_0_0_0_1px_#c6bdab,0_4px_12px_rgba(60,47,30,0.07)] enabled:hover:bg-[#fffdf7]",
  danger:
    "bg-[#f2ddd5] text-[#7a2f28] shadow-[inset_0_0_0_1px_#d8a49a] enabled:hover:bg-[#efd0c6]",
  quiet:
    "bg-transparent text-ink-soft shadow-none enabled:hover:bg-[rgba(67,58,45,0.08)] enabled:hover:text-ink",
} as const;

export function Button({
  variant = "primary",
  size = "normal",
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: keyof typeof buttonVariants;
  size?: "normal" | "small";
}) {
  return (
    <button
      className={`${buttonBase} ${buttonVariants[variant]} ${size === "small" ? "min-h-9 px-3 text-[13px]" : ""} ${className}`}
      {...props}
    />
  );
}

export function Surface({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <section
      className={`rounded-md bg-paper shadow-[inset_0_0_0_1px_rgba(83,70,49,0.13),var(--shadow-low)] ${className}`}
    >
      {children}
    </section>
  );
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description: string;
  actions?: ReactNode;
}) {
  return (
    <header className="mb-7 flex items-end justify-between gap-6 max-sm:flex-col max-sm:items-start max-sm:gap-4">
      <div>
        <h1 className="m-0 font-display text-[clamp(30px,4vw,48px)] leading-[1.02] font-[800] tracking-[-0.025em]">
          {title}
        </h1>
        <p className="mt-2 mb-0 max-w-[65ch] leading-[1.55] text-ink-soft">
          {description}
        </p>
      </div>
      {actions ? (
        <div className="flex flex-wrap justify-end gap-2.5 max-sm:justify-start">
          {actions}
        </div>
      ) : null}
    </header>
  );
}

export function EmptyState({
  title,
  description,
  icon,
}: {
  title: string;
  description: string;
  icon?: ReactNode;
}) {
  return (
    <div className="px-7 py-[42px] text-center">
      <div className="mx-auto mb-3.5 grid size-12 place-items-center rounded-xl bg-[#e7dfcd] text-earth">
        {icon ?? <Box size={23} />}
      </div>
      <h2 className="mb-[7px] font-display text-xl leading-[1.2] font-[780]">
        {title}
      </h2>
      <p className="mx-auto max-w-[52ch] leading-normal text-ink-soft">
        {description}
      </p>
    </div>
  );
}

export function Notice({
  children,
  tone = "info",
}: {
  children: ReactNode;
  tone?: "info" | "warning" | "danger";
}) {
  const Icon =
    tone === "danger" ? AlertCircle : tone === "warning" ? TriangleAlert : Info;
  const toneClass =
    tone === "danger"
      ? "bg-[#f0d8d2] text-[#7d3029]"
      : tone === "warning"
        ? "bg-[#f1e3c8] text-[#72501f]"
        : "bg-[#dfeaf0] text-[#2c5265]";

  return (
    <div
      className={`flex items-start gap-[11px] rounded-[10px] px-4 py-3.5 leading-[1.45] [&>svg]:mt-0.5 [&>svg]:shrink-0 ${toneClass}`}
      role={tone === "danger" ? "alert" : undefined}
    >
      <Icon size={18} />
      <div>{children}</div>
    </div>
  );
}

export function SwitchField({
  checked,
  onCheckedChange,
  label,
  description,
  disabled,
}: {
  checked: boolean;
  onCheckedChange: (value: boolean) => void;
  label: string;
  description: string;
  disabled?: boolean;
}) {
  return (
    <label className="flex min-h-[54px] items-center justify-between gap-5">
      <span className="grid gap-[3px]">
        <strong className="text-sm">{label}</strong>
        <span className="text-xs leading-[1.4] text-ink-soft">
          {description}
        </span>
      </span>
      <Switch.Root
        className="group h-[25px] w-11 shrink-0 cursor-pointer rounded-full border-0 bg-[#a7a292] p-[3px] transition-[background-color] duration-150 data-[state=checked]:bg-grass disabled:cursor-not-allowed disabled:opacity-45"
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
        aria-label={label}
      >
        <Switch.Thumb className="block size-[19px] rounded-[6px] bg-[#fffdf8] shadow-[0_2px_5px_rgba(0,0,0,0.22)] transition-transform duration-150 group-data-[state=checked]:translate-x-[19px]" />
      </Switch.Root>
    </label>
  );
}

export function Field({
  label,
  error,
  help,
  children,
}: {
  label: string;
  error?: string;
  help?: string;
  children: ReactNode;
}) {
  return (
    <label className="grid gap-[7px]">
      <span className="text-[13px] font-[760] text-[#4d4b42]">{label}</span>
      {children}
      {error ? (
        <span className="text-xs leading-[1.35] text-danger" role="alert">
          {error}
        </span>
      ) : help ? (
        <span className="text-xs leading-[1.4] text-ink-soft">{help}</span>
      ) : null}
    </label>
  );
}

const controlBase =
  "min-h-11 w-full rounded-sm border border-[#bdb5a4] bg-[#fffdf8] px-3 py-2.5 text-ink disabled:cursor-not-allowed disabled:bg-[#e9e3d5] disabled:text-[#777366]";

export const controlStyles = {
  input: controlBase,
  select: controlBase,
  textarea: `${controlBase} min-h-[94px] resize-y`,
};

export function ConfirmDialog({
  trigger,
  title,
  description,
  confirmLabel,
  onConfirm,
  danger = false,
  disabled = false,
}: {
  trigger: ReactNode;
  title: string;
  description: string;
  confirmLabel: string;
  onConfirm: () => Promise<void> | void;
  danger?: boolean;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild disabled={disabled}>
        {trigger}
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-80 animate-[fade-in_140ms_ease-out] bg-[rgba(35,31,25,0.56)]" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-81 w-[min(480px,calc(100vw-32px))] -translate-x-1/2 -translate-y-1/2 rounded-2xl bg-paper p-6 shadow-high">
          <Dialog.Title className="font-display text-[23px] leading-[1.15] font-[800]">
            {title}
          </Dialog.Title>
          <Dialog.Description className="mt-2.5 leading-[1.55] text-ink-soft">
            {description}
          </Dialog.Description>
          <div className="mt-6 flex justify-end gap-[9px] max-sm:flex-col-reverse max-sm:[&>button]:w-full">
            <Dialog.Close asChild>
              <Button variant="secondary" disabled={busy}>
                Cancel
              </Button>
            </Dialog.Close>
            <Button
              variant={danger ? "danger" : "primary"}
              disabled={busy}
              onClick={async () => {
                setBusy(true);
                try {
                  await onConfirm();
                  setOpen(false);
                } finally {
                  setBusy(false);
                }
              }}
            >
              {busy ? "Working…" : confirmLabel}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
