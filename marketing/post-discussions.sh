#!/usr/bin/env bash
# Publish the three seed Discussions to gambletan/cortex.
# Run this yourself when you're ready — it posts publicly under your GitHub identity.
#   gh auth switch --user gambletan   # ensure the owner account is active
#   bash marketing/post-discussions.sh
set -euo pipefail

REPO="R_kgDORjfAIg"
SHOW_AND_TELL="DIC_kwDORjfAIs4C4ruc"
IDEAS="DIC_kwDORjfAIs4C4rub"
QA="DIC_kwDORjfAIs4C4rua"

DIR="$(cd "$(dirname "$0")/discussions" && pwd)"
MUT='mutation($repo:ID!,$cat:ID!,$title:String!,$body:String!){createDiscussion(input:{repositoryId:$repo,categoryId:$cat,title:$title,body:$body}){discussion{url}}}'

post() {
  local file="$1" cat="$2"
  # First line is "# Title"; body is everything after the "> Category:" line.
  local title body
  title="$(head -1 "$file" | sed 's/^# //')"
  body="$(awk 'NR>1 && $0 !~ /^> Category:/' "$file" | sed '/./,$!d')"
  gh api graphql -f query="$MUT" -f repo="$REPO" -f cat="$cat" \
    -f title="$title" -f body="$body" \
    --jq '.data.createDiscussion.discussion.url'
}

echo "Posting Show and tell..."; post "$DIR/01-show-and-tell.md" "$SHOW_AND_TELL"
echo "Posting Ideas...";        post "$DIR/02-roadmap-ideas.md" "$IDEAS"
echo "Posting Q&A...";          post "$DIR/03-locomo-repro.md"  "$QA"
echo "Done."
