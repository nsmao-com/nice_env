import { runScopeHandlers, type EditorView, type Panel } from "@codemirror/view";
import { SearchQuery, closeSearchPanel, findNext, findPrevious, getSearchQuery, replaceAll, replaceNext, selectMatches, setSearchQuery } from "@codemirror/search";

/** CodeMirror owns this panel's lifecycle; keep inputs mounted while its query changes. */
export function editorSearchPanel(view: EditorView, english: boolean): Panel {
  const text = english ? {
    title: "Find & replace", find: "Find in this file", replace: "Replace with… (empty to delete)",
    previous: "Previous", next: "Next", all: "Select all", replaceOne: "Replace", replaceAll: "Replace all",
    matchCase: "Match case", word: "Whole word", regexp: "Regular expression", close: "Close search",
    empty: "Enter text to search", invalid: "Invalid regular expression", none: "No matches",
    hint: "Enter next · Shift+Enter previous · Esc close", matches: "matches",
  } : {
    title: "查找与替换", find: "在当前文件中查找", replace: "替换为…（留空即删除）",
    previous: "上一处", next: "下一处", all: "选中全部", replaceOne: "替换当前", replaceAll: "全部替换",
    matchCase: "区分大小写", word: "全字匹配", regexp: "正则表达式", close: "关闭搜索",
    empty: "输入查找内容", invalid: "正则表达式无效", none: "没有匹配结果",
    hint: "Enter 下一处 · Shift+Enter 上一处 · Esc 关闭", matches: "处匹配",
  };
  const doc = view.dom.ownerDocument;
  const element = (tag: string, className: string) => {
    const node = doc.createElement(tag);
    node.className = className;
    return node;
  };
  const dom = element("div", "nsb-editor-search");
  dom.setAttribute("role", "search");
  dom.setAttribute("aria-label", text.title);
  const header = element("div", "nsb-search-header");
  const title = element("strong", "nsb-search-title"); title.textContent = text.title;
  const status = element("span", "nsb-search-status");
  status.setAttribute("role", "status"); status.setAttribute("aria-live", "polite");
  const button = (label: string, run: () => void) => {
    const node = doc.createElement("button");
    node.type = "button"; node.className = "nsb-search-button";
    node.textContent = label; node.addEventListener("click", run);
    return node;
  };
  const close = button("×", () => { closeSearchPanel(view); view.focus(); });
  close.classList.add("nsb-search-close"); close.setAttribute("aria-label", text.close);
  const input = (placeholder: string, name: string) => {
    const node = doc.createElement("input");
    node.className = "nsb-search-input"; node.type = "text"; node.name = name;
    node.placeholder = placeholder; node.setAttribute("aria-label", placeholder);
    node.setAttribute("form", ""); node.autocomplete = "off"; node.spellcheck = false;
    node.addEventListener("input", commit);
    return node;
  };
  const searchField = input(text.find, "search"); searchField.setAttribute("main-field", "true");
  const replaceField = input(text.replace, "replace");
  const previous = button(text.previous, () => { findPrevious(view); });
  const next = button(text.next, () => { findNext(view); });
  const all = button(text.all, () => { selectMatches(view); view.focus(); });
  const replaceOne = button(text.replaceOne, () => { replaceNext(view); });
  const replaceEvery = button(text.replaceAll, () => { replaceAll(view); });
  const searchRow = element("div", "nsb-search-row");
  const searchActions = element("div", "nsb-search-actions"); searchActions.append(previous, next, all);
  searchRow.append(searchField, searchActions);
  const replaceRow = element("div", "nsb-search-row");
  const replaceActions = element("div", "nsb-search-actions"); replaceActions.append(replaceOne, replaceEvery);
  replaceRow.append(replaceField, replaceActions);
  const options = element("div", "nsb-search-options");
  const toggle = (label: string) => {
    const node = button(label, () => {
      node.setAttribute("aria-pressed", String(node.getAttribute("aria-pressed") !== "true"));
      commit();
    });
    node.classList.add("nsb-search-toggle"); node.setAttribute("aria-pressed", "false");
    options.append(node);
    return node;
  };
  const matchCase = toggle(text.matchCase), word = toggle(text.word), regexp = toggle(text.regexp);
  const hint = element("span", "nsb-search-hint"); hint.textContent = text.hint;
  options.append(hint); header.append(title, status, close);
  dom.append(header, searchRow, replaceRow, options);

  function commit() {
    const query = new SearchQuery({
      search: searchField.value, replace: replaceField.value,
      caseSensitive: matchCase.getAttribute("aria-pressed") === "true",
      wholeWord: word.getAttribute("aria-pressed") === "true",
      regexp: regexp.getAttribute("aria-pressed") === "true",
      literal: getSearchQuery(view.state).literal,
    });
    if (!query.eq(getSearchQuery(view.state))) view.dispatch({ effects: setSearchQuery.of(query) });
  }
  function refresh() {
    const query = getSearchQuery(view.state);
    if (searchField.value !== query.search) searchField.value = query.search;
    if (replaceField.value !== query.replace) replaceField.value = query.replace;
    matchCase.setAttribute("aria-pressed", String(query.caseSensitive));
    word.setAttribute("aria-pressed", String(query.wholeWord));
    regexp.setAttribute("aria-pressed", String(query.regexp));
    replaceRow.hidden = view.state.readOnly;
    let count = 0, active = 0, capped = false;
    if (query.valid) {
      const cursor = query.getCursor(view.state);
      const selected = view.state.selection.main;
      for (let result = cursor.next(); !result.done; result = cursor.next()) {
        if (count === 10000) { capped = true; break; }
        count++;
        if (result.value.from === selected.from && result.value.to === selected.to) active = count;
      }
    }
    const invalid = !!query.search && !query.valid;
    searchField.setAttribute("aria-invalid", String(invalid));
    status.textContent = !query.search ? text.empty : invalid ? text.invalid : !count ? text.none
      : `${active ? `${active} / ` : ""}${count}${capped ? "+" : ""} ${text.matches}`;
    status.classList.toggle("nsb-search-error", invalid || (!!query.search && !count));
    for (const action of [previous, next, all]) action.disabled = !count;
    for (const action of [replaceOne, replaceEvery]) action.disabled = !count || view.state.readOnly;
  }
  dom.addEventListener("keydown", (event) => {
    if (event.isComposing) return;
    // Keep Escape inside a config dialog: close search first, not the whole dialog.
    if (event.key === "Escape") {
      event.preventDefault(); event.stopPropagation(); closeSearchPanel(view); view.focus();
    } else if (runScopeHandlers(view, event, "search-panel")) {
      event.preventDefault();
    } else if (event.key === "Enter" && (event.target === searchField || event.target === replaceField)) {
      event.preventDefault();
      if (event.target === replaceField && !view.state.readOnly) replaceNext(view);
      else (event.shiftKey ? findPrevious : findNext)(view);
    }
  });
  refresh();
  return { dom, top: true, mount() { searchField.focus(); searchField.select(); }, update(update) {
    if (update.docChanged || update.selectionSet || update.transactions.some(tr => tr.reconfigured || tr.effects.some(effect => effect.is(setSearchQuery)))) refresh();
  } };
}
