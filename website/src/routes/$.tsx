import { ButtonLink } from '#/components/ui/button'
import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/$')({ component: NotFound })

function NotFound() {
  return (
    <main className="not-found-page">
      <div className="not-found-card">
        <span className="section-kicker">404 / route not found</span>
        <h1>That path is not part of Pix.</h1>
        <p>The host is small on purpose. Head back to the homepage to see how it works.</p>
        <ButtonLink href="/" variant="primary">Back to Pix</ButtonLink>
      </div>
    </main>
  )
}
