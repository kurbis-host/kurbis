import { COPY } from "../../lib/landing/copy";
import { LeverControl, ShortcutControl } from "./mirror-controls";
import { NameField } from "./name-field";

/**
 * Everything the film says, as the page's own HTML in reading order. While the film
 * plays this is what a screen reader and a search engine read, and its controls are what
 * a keyboard uses; where the film cannot play, it is the page.
 */
export function Mirror() {
  const { sky, surface, dig, cave, dungeon, spawner, nether } = COPY;
  return (
    <article className="landing-mirror">
      <section aria-labelledby="landing-sky">
        <div className="landing-text">
          <h1 id="landing-sky">{sky.title.join(" ")}</h1>
          <p>{sky.caption.join(" ")}.</p>
        </div>
        <ShortcutControl />
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

      <section aria-labelledby="landing-dungeon">
        <div className="landing-text">
          <h2 id="landing-dungeon">{dungeon.headline}</h2>
          <dl>
            {[...dungeon.ores, dungeon.backups].map(([name, says]) => (
              <div key={name}>
                <dt>{name}</dt>
                <dd>{says}</dd>
              </div>
            ))}
          </dl>
        </div>
      </section>

      <section aria-labelledby="landing-spawner">
        <div className="landing-text">
          <h2 id="landing-spawner">{spawner.headline}</h2>
          <p>{spawner.small}</p>
          <ul>
            {spawner.asks.map(([ask, answer]) => (
              <li key={ask}>
                <code>{ask}</code> {answer}
              </li>
            ))}
          </ul>
        </div>
      </section>

      <section id="name" aria-labelledby="landing-nether">
        <div className="landing-text">
          <h2 id="landing-nether">{nether.headline}</h2>
          <p>{nether.caption}</p>
        </div>
        <NameField />
        <p>
          <a href="/login" className="landing-chip">
            Sign in
          </a>
        </p>
      </section>
    </article>
  );
}
