import { Fragment } from "react";
import { COPY } from "../../lib/landing/copy";
import { BarControls, LeverControl } from "./mirror-controls";
import { NameField, NameProvider } from "./name-field";

/**
 * Everything the film says, as the page's own HTML in reading order. While the film
 * plays this is what a screen reader and a search engine read, and its controls are what
 * a keyboard uses; where the film cannot play, it is the page.
 */
export function Mirror() {
  const { sky, hood, surface, dig, cave, homestead, nether, ending, credits, footer } = COPY;
  return (
    <article className="landing-mirror">
      <NameProvider>
        <BarControls />
        <section aria-labelledby="landing-sky">
          <div className="landing-text">
            <h1 id="landing-sky">{sky.title.join(" ")}</h1>
            <p>{sky.caption.join(" ")}.</p>
          </div>
          <NameField where="sky" />
          {/* What the splash over a free name says when the pointer finds it, and its link (a keyboard's way to it). */}
          <div className="landing-text">
            <p>{hood.lines.join(" ")}</p>
          </div>
          <a href={hood.link[1]} target="_blank" rel="noopener noreferrer" className="landing-chip">
            {hood.link[0]}
          </a>
        </section>

        <section aria-labelledby="landing-surface">
          <div className="landing-text">
            <h2 id="landing-surface">{surface.caption}</h2>
            <p>
              Your server gets its own address: {surface.names[0]}
              {surface.host}, or whatever you call it.
            </p>
            <p>
              {surface.editions} {surface.tags.join(" and ")} players join the same world.
            </p>
          </div>
        </section>

        <section aria-labelledby="landing-dig">
          <div className="landing-text">
            <h2 id="landing-dig">{dig.facts[0][0]}</h2>
            <p>{dig.facts[0][1]}</p>
            <p>{dig.facts[1].join(" ")}</p>
          </div>
        </section>

        <section aria-labelledby="landing-cave">
          <div className="landing-text">
            <h2 id="landing-cave">{cave.awake.join(" ")}</h2>
          </div>
          <LeverControl />
        </section>

        {homestead.heads.map((head, k) => (
          <section key={head.join(" ")} aria-labelledby={`landing-home-${k}`}>
            <div className="landing-text">
              <h2 id={`landing-home-${k}`}>{head.join(" ")}</h2>
              <p>{homestead.said[k]}</p>
            </div>
          </section>
        ))}

        <section id="name" aria-labelledby="landing-nether">
          <div className="landing-text">
            <h2 id="landing-nether">{nether.headline}</h2>
            <p>{nether.caption}</p>
          </div>
          <NameField where="nether" />
        </section>

        <section aria-labelledby="landing-ending">
          <div className="landing-text">
            <h2 id="landing-ending">{ending.title}</h2>
            <ul>
              {ending.specs.map(({ said }) => (
                <li key={said}>{said}</li>
              ))}
            </ul>
            <h3>{ending.book}</h3>
            <dl>
              {ending.faq.map(([question, answer]) => (
                <div key={question}>
                  <dt>{question}</dt>
                  <dd>{answer}</dd>
                </div>
              ))}
            </dl>
          </div>
        </section>

        {/* The credits the film rolls at its end: the page's own story, then its last links and line. */}
        <footer className="landing-footer">
          <div className="landing-text">
            {credits.parts.map(([heading, said]) => (
              <Fragment key={heading}>
                <h2>{heading}</h2>
                {said.map((text) => (
                  <p key={text}>{text}</p>
                ))}
              </Fragment>
            ))}
            <h2>{credits.community}</h2>
          </div>
          {credits.links.map(([label, href]) => (
            <a key={href} href={href} target="_blank" rel="noopener noreferrer" className="landing-chip">
              {label}
            </a>
          ))}
          {/* (The page's own, after them: Kurbis on GitHub, in a tab of its own as Pumpkin's places are, and the legal pages.) */}
          {footer.links.map(([label, href]) => (
            <a key={href} href={href} {...(href.startsWith("https:") ? { target: "_blank", rel: "noopener noreferrer" } : {})} className="landing-chip">
              {label}
            </a>
          ))}
          <p className="landing-text">{footer.disclaimer}</p>
        </footer>
      </NameProvider>
    </article>
  );
}
