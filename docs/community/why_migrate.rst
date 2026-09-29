Why should I migrate to Charset-Normalizer?
===========================================

There is so many reason to migrate your current project. Here are some of them:

- Remove ANY license ambiguity/restriction for projects bundling Chardet (even indirectly).
- Actively maintained, open to contributors. Always was.
- Have the backward compatible function ``detect`` that come from Chardet.
- It is, for the first time, really universal! As there is no specific probe per charset.
- The package size is X2~X3 lower than Chardet's (7.0)! (Depends on your arch/OS)
- 2X Faster than Chardet in p99 latency. 10X faster with large content. When Chardet is uncapped (i.e. max_bytes).
- Faster than older C/C++ bind of uchardet. There is no more need to use those wrapper.
- Truly protect you from decoding error! E.g. Never get UnicodeDecodeError, ever!
- Propose much more options/public kwargs to tweak the detection as you sees fit!

And much more..! What are you waiting for? Upgrade now and give us a feedback. (Even if negative)
