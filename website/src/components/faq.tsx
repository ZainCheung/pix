const questions = [
  {
    question: 'Does Pix replace Pi?',
    answer: 'No. Pi remains the coding agent running on your computer. Pix gives you a remote interface to start, resume, and control those Pi sessions from your phone.',
  },
  {
    question: 'Can I continue my existing Pi sessions?',
    answer: 'Yes. Pix works with the native Pi sessions already stored on your computer, so you can pick up where you left off instead of starting over.',
  },
  {
    question: 'Does my computer need to stay on?',
    answer: 'Yes. Pi runs on your Mac or Linux machine, so that machine needs to be running and reachable while you use Pix remotely.',
  },
  {
    question: 'Can I use Pix away from home?',
    answer: 'Yes. Pix connects directly when your devices are on the same network and can use an encrypted relay when you are away.',
  },
  {
    question: 'Does my code leave my computer?',
    answer: 'Your repositories, credentials, Pi processes, and session data stay on your computer. Pix remotely controls the host instead of moving your workspace into a hosted environment.',
  },
  {
    question: 'What do I need to use Pix?',
    answer: 'A Mac or Linux machine with Pi installed and working, the Pix Host on that machine, and the Pix app on your iPhone.',
  },
  {
    question: 'Do I need a Pix account?',
    answer: 'No. Pix uses explicit device pairing instead of a hosted account. You authorize each client and can revoke it later.',
  },
]

export function FAQ() {
  return (
    <section className="faq-v2" aria-labelledby="faq-heading">
      <div className="faq-content-v2">
        <div className="section-label-v2">Questions</div>
        <h2 id="faq-heading">A few common questions</h2>
        <div className="faq-list-v2">
          {questions.map((item) => (
            <details className="faq-item-v2" key={item.question}>
              <summary>
                <span>{item.question}</span>
                <span className="faq-plus-v2" aria-hidden="true">+</span>
              </summary>
              <p>{item.answer}</p>
            </details>
          ))}
        </div>
      </div>
    </section>
  )
}
