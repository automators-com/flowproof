# The destructive half of the `assert_no_side_effect` falsifiability pair
# (issue #465): one model call through the proxy, one real workspace unlink.
import json
import os
import urllib.request

with open("victim.csv", "w") as f:
    f.write("doomed")
os.unlink("victim.csv")

base = os.environ["OPENAI_BASE_URL"]
payload = json.dumps({"model": "gpt-4o", "messages": [
    {"role": "user", "content": os.environ["FLOWPROOF_PROMPT"]}]}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                             headers={"content-type": "application/json"})
urllib.request.urlopen(req).read()
