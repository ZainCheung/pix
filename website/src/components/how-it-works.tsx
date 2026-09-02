export function HowItWorks() {
  return (
    <section className="how-model-v2" id="how" aria-labelledby="how-heading">
      <div className="how-model-intro-v2">
        <div className="section-label-v2">How Pix works</div>
        <h2 id="how-heading">Two pieces. One Pi.</h2>
        <p>
          Pix runs on your computer and on your iPhone. Pi, your code, and
          your sessions stay on the computer.
        </p>
      </div>

      <div className="how-model-frame-v2">
        <article className="how-model-node-v2">
          <small>Computer</small>
          <strong>Pix for Mac / Linux</strong>
          <p>Pi runs here.</p>
        </article>
        <div className="how-model-link-v2" aria-hidden="true">
          <span className="how-model-link-line-v2" />
          <span>pair once</span>
          <span className="how-model-link-line-v2" />
        </div>
        <article className="how-model-node-v2">
          <small>iPhone</small>
          <strong>Pix for iPhone</strong>
          <p>Control Pi here.</p>
        </article>
      </div>

      <p className="how-model-close-v2">
        Install Pix on both devices, pair them once, and you&apos;re ready.
      </p>
    </section>
  )
}
