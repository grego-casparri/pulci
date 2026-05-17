"""
Email delivery helpers.
"""

from __future__ import annotations

import smtplib
from email.message import EmailMessage


# B006: mutable default argument — recipients list mutated across calls if caller reuses the default
def build_message(
    subject: str,
    body: str,
    sender: str = "noreply@example.com",
    recipients: list[str] = [],
) -> EmailMessage:
    msg = EmailMessage()
    msg["Subject"] = subject
    msg["From"] = sender
    msg["To"] = ", ".join(recipients)
    msg.set_content(body)
    return msg


def send(msg: EmailMessage, host: str = "localhost", port: int = 25) -> None:
    with smtplib.SMTP(host, port) as smtp:
        smtp.send_message(msg)


def plain_text_preview(msg: EmailMessage, max_chars: int = 200) -> str:
    body = msg.get_body()
    content = body.get_content() if body else ""
    return content[:max_chars].strip()
