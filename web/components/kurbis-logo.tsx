import type { ImgHTMLAttributes } from "react";

export interface KurbisLogoProps
  extends Omit<ImgHTMLAttributes<HTMLImageElement>, "src"> {
  size?: number;
}

export function KurbisLogo({
  size = 32,
  alt = "",
  className,
  style,
  "aria-hidden": ariaHidden = true,
  ...props
}: KurbisLogoProps) {
  return (
    <img
      src="/kurbis.svg"
      alt={alt}
      aria-hidden={ariaHidden}
      width={size}
      height={size}
      className={className}
      style={{
        imageRendering: "pixelated",
        display: "inline-block",
        verticalAlign: "middle",
        ...style,
      }}
      {...props}
    />
  );
}
