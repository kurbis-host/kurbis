"use client";

import { useState } from "react";

export function VoxelDemo() {
  const [awake, setAwake] = useState(false);

  return (
    <div className="relative grid min-h-[520px] place-items-center max-[900px]:min-h-[500px] max-[560px]:min-h-[390px]">
      <div
        className="absolute inset-[20px_0_34px_24px] overflow-hidden rounded-[26px] bg-[#9cc5d3] shadow-[inset_0_0_0_1px_rgba(44,91,107,0.16),var(--shadow-high)] after:absolute after:right-0 after:bottom-0 after:left-0 after:h-[35%] after:bg-[#cab28d] after:content-[''] max-[560px]:inset-[0_0_38px] max-[560px]:rounded-[18px]"
        aria-hidden="true"
      >
        <span
          className={`absolute top-[55px] right-[68px] size-14 rounded-[9px] bg-[#f2c66f] transition-[transform,background-color] duration-700 ease-[cubic-bezier(.2,.8,.2,1)] ${awake ? "-translate-y-1 rotate-[8deg] bg-[#ffd274]" : ""}`}
        />
        <span className="absolute top-[104px] left-[66px] h-[22px] w-[85px] bg-white/70 shadow-[22px_-18px_0_rgba(255,255,255,0.7),52px_4px_0_rgba(255,255,255,0.7)]" />

        <div
          className={`absolute top-[52%] left-1/2 h-[250px] w-[290px] [transform-style:preserve-3d] transition-transform duration-700 ease-[cubic-bezier(.2,.8,.2,1)] max-[560px]:[transform:translate(-50%,-48%)_rotateX(56deg)_rotateZ(-45deg)_scale(.74)] ${
            awake
              ? "[transform:translate(-50%,-52%)_rotateX(56deg)_rotateZ(-45deg)_scale(1.03)] max-[560px]:[transform:translate(-50%,-50%)_rotateX(56deg)_rotateZ(-45deg)_scale(.78)]"
              : "[transform:translate(-50%,-50%)_rotateX(56deg)_rotateZ(-45deg)]"
          }`}
        >
          <span className="absolute top-3 left-[30px] size-[230px] bg-[#6f8d44] shadow-[22px_22px_0_#5c4432,44px_44px_0_#493729]" />
          <span className="absolute top-[82px] left-[52px] size-[74px] bg-[#5e9bb2] shadow-[inset_-12px_-12px_0_rgba(37,96,117,0.22)]" />
          <span className="absolute top-[66px] left-[145px] h-16 w-[74px] bg-[#bd8b56] shadow-[10px_10px_0_#7f5637]" />
          <span className="absolute top-[42px] left-[138px] h-[30px] w-[88px] bg-[#844a36] shadow-[8px_8px_0_#63372b]" />
          <span className="absolute top-[34px] left-[72px] size-9 bg-[#3f703d] shadow-[8px_8px_0_#315b31]" />
          <span className="absolute top-[156px] left-[174px] size-9 bg-[#3f703d] shadow-[8px_8px_0_#315b31]" />
          <span className="absolute top-[158px] left-[120px] size-[30px] rounded-[5px] bg-pumpkin shadow-[6px_6px_0_var(--color-pumpkin-dark)]" />
        </div>

        <span
          className={`pointer-events-none absolute size-[250px] rounded-full border-2 border-[rgba(255,197,105,0.65)] opacity-0 [transform:scale(.4)] ${awake ? "animate-[wake-pulse_800ms_ease-out]" : ""}`}
        />
      </div>

      <div
        className={`pointer-events-none absolute inset-[20px_0_34px_24px] rounded-[26px] bg-[rgba(34,42,54,0.48)] transition-opacity duration-500 max-[560px]:inset-[0_0_38px] max-[560px]:rounded-[18px] ${awake ? "opacity-0" : ""}`}
        aria-hidden="true"
      >
        <span
          className={`absolute top-[52px] right-16 size-[50px] rounded-sm bg-[#e8dfc6] shadow-[inset_-15px_8px_0_#b7bdbe] transition-[opacity,transform] duration-[550ms] ${awake ? "opacity-0" : ""}`}
        />
      </div>

      <div
        className="absolute right-0 bottom-0 w-[min(310px,74%)] rounded-md bg-paper p-4 shadow-high max-[560px]:w-[84%]"
        aria-live="polite"
      >
        <div className="flex items-center justify-between gap-4">
          <div>
            <strong className="font-display text-[17px] leading-[1.2] font-[780]">
              {awake ? "World online" : "World sleeping"}
            </strong>
            <br />
            <span className="text-xs text-ink-soft">
              {awake ? "Gateway routing players" : "No worker allocated"}
            </span>
          </div>
          <span className="text-xs text-ink-soft">{awake ? "LIVE" : "SAFE"}</span>
        </div>
        <button
          className="mt-3.5 min-h-11 w-full cursor-pointer rounded-sm border-0 bg-[#302b25] font-[760] text-white"
          onClick={() => setAwake((value) => !value)}
        >
          {awake ? "Let the world sleep" : "Wake the world"}
        </button>
      </div>
    </div>
  );
}
