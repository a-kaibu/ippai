// When the corpus' story clicks (every 2 s for STORY seconds) take a screenshot: every 20 s and
// at the end. check.py grades a title `story` only with two different story screens, which the
// default 30 s gave just one of while the shots were every 20 s alone. tests/corpus/winplay.py
// has the same rule for Windows (story_shot); tests/corpus/winplay_test.py checks both agree.
export const storyShot = (s, story) => s % 20 === 18 || s + 2 >= story;
