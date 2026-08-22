const questions = [
  {
    question: 'Where does my code run?',
    answer: 'On the machine where you run the Pix Host. Pix does not move your workspace into a hosted environment or create a second session store.',
  },
  {
    question: 'What happens when I am on the same network?',
    answer: 'The client discovers the host over Bonjour and prefers a direct TCP connection. The encrypted Pix wire protocol is the same on that path.',
  },
  {
    question: 'Can the relay read my prompts or files?',
    answer: 'No. The relay forwards authenticated opaque encrypted frames and does not decrypt, parse, queue, or persist application payloads.',
  },
  {
    question: 'What does install.sh install?',
    answer: 'On Linux it downloads the matching release archive and installs the pix CLI into ~/.local/bin. On Apple Silicon it also installs Pix.app into ~/Applications.',
  },
  {
    question: 'Do I need an account?',
    answer: 'No. Pix uses explicit workspace authorization and device pairing instead of a hosted account layer.',
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
