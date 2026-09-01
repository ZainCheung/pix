import { ButtonLink } from '#/components/ui/button'
import { START_PATH } from '#/lib/install'

const steps = [
  {
    n: '01',
    title: 'Install on your computer',
    body: 'Pix for Mac or Linux. Pi keeps running here.',
  },
  {
    n: '02',
    title: 'Install on your iPhone',
    body: 'Pix for iPhone. This is how you control Pi.',
  },
  {
    n: '03',
    title: 'Pair once',
    body: 'Connect the two devices. After that, you are ready.',
  },
]

export function GetStarted() {
  return (
    <section className="start-overview-v2" id="get-started" aria-labelledby="start-heading">
      <div className="start-overview-intro-v2">
        <div className="section-label-v2">Get started</div>
        <h2 id="start-heading">Three steps. Two devices.</h2>
        <p>Pix has two parts. Install both, pair them once, and you can use Pi from your phone.</p>
      </div>

      <div className="need-v2" aria-labelledby="need-heading">
        <h3 id="need-heading">What you need</h3>
        <div className="need-grid-v2">
          <p>
            <strong>A Mac or Linux computer</strong>
            with Pi already installed and working
          </p>
          <p>
            <strong>An iPhone or iPad</strong>
            to install Pix for iPhone
          </p>
        </div>
        <p className="need-close-v2">That&apos;s it.</p>
      </div>

      <ol className="start-steps-v2">
        {steps.map((step) => (
          <li key={step.n}>
            <span>{step.n}</span>
            <div>
              <strong>{step.title}</strong>
              <p>{step.body}</p>
            </div>
          </li>
        ))}
      </ol>

      <div className="start-overview-cta-v2">
        <ButtonLink href={START_PATH} variant="primary">
          Set up Pix
        </ButtonLink>
        <a href="/docs">View docs</a>
      </div>
    </section>
  )
}
