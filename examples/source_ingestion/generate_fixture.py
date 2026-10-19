"""Generate a deterministic Zendesk-shaped shadow fixture, never production data.

Run from the repository root:
  python3 -m examples.source_ingestion.generate_fixture /tmp/tickets.jsonl --tickets 10000
Each ticket precedes its comments. Repeated provider IDs across tenants are deliberate.
"""
import argparse
from collections import Counter
import json
from pathlib import Path


def generate(file, tickets=10000, comments_per_ticket=2, body_bytes=1024):
    if any(type(n) is not int for n in (tickets, comments_per_ticket, body_bytes)):
        raise ValueError("fixture sizes must be integers")
    if not 1 <= tickets <= 100000 or not 0 <= comments_per_ticket <= 8 or not 0 <= body_bytes <= 8192:
        raise ValueError("tickets: 1–100000, comments: 0–8, body-bytes: 0–8192")
    statuses = Counter()
    priorities = Counter()
    with Path(file).open("x", encoding="utf-8", newline="\n") as stream:
        for i in range(tickets):
            scope = {"tenant_id": f"tenant-{i % 2}", "connector_id": "zendesk-main"}
            external_id = str(i // 2)
            status = ["open", "pending", "solved"][i % 3]
            priority = "high" if i % 5 == 0 else "normal"
            statuses[status] += 1
            priorities[priority] += 1
            ticket = dict(scope, record_type="ticket", external_id=external_id,
                          subject=f"Synthetic support request {i}", status=status,
                          priority=priority, tags=["synthetic", f"bucket-{i % 7}"])
            stream.write(json.dumps(ticket, ensure_ascii=False) + "\n")
            for j in range(comments_per_ticket):
                prefix = f"Synthetic comment {i}/{j}: "
                body = (prefix + "x" * body_bytes)[:body_bytes]
                comment = dict(scope, record_type="comment", external_id=f"{external_id}-{j}",
                               ticket_id=external_id, body=body, is_public=j % 2 == 0)
                stream.write(json.dumps(comment, ensure_ascii=False) + "\n")
    return {"synthetic": True, "tickets": tickets,
            "comments": tickets * comments_per_ticket,
            "relationships": tickets * comments_per_ticket,
            "status_counts": dict(statuses), "priority_counts": dict(priorities),
            "comment_body_characters": body_bytes, "source_bytes": Path(file).stat().st_size}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file")
    parser.add_argument("--tickets", type=int, default=10000)
    parser.add_argument("--comments-per-ticket", type=int, default=2)
    parser.add_argument("--body-bytes", type=int, default=1024)
    args = parser.parse_args()
    print(json.dumps(generate(args.file, args.tickets, args.comments_per_ticket, args.body_bytes), indent=2))


if __name__ == "__main__":
    main()
