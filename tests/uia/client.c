/* A UI Automation client for the provider probe: reads the probe window's
   tree, acts on each control, waits for "phase2" on stdin, then reads the
   results back and moves focus to the button. Prints one line per fact.
   With `--tree TITLE` it prints the tree of the window titled TITLE
   instead, one element per line, indented by depth. */
#define COBJMACROS
#include <windows.h>
#include <ole2.h>
#include <uiautomation.h>
#include <stdio.h>

static IUIAutomation *uia;

static void name_of(IUIAutomationElement *e, char *out, int size) {
    BSTR name = NULL;
    out[0] = 0;
    if (SUCCEEDED(IUIAutomationElement_get_CurrentName(e, &name)) && name) {
        WideCharToMultiByte(CP_UTF8, 0, name, -1, out, size, NULL, NULL);
        SysFreeString(name);
    }
}

static IUIAutomationElement *child(IUIAutomationElement *root, const char *want) {
    IUIAutomationCondition *all;
    IUIAutomationElementArray *found;
    int n = 0;
    IUIAutomation_CreateTrueCondition(uia, &all);
    if (FAILED(IUIAutomationElement_FindAll(root, TreeScope_Children, all, &found)) || !found) return NULL;
    IUIAutomationElementArray_get_Length(found, &n);
    for (int i = 0; i < n; i++) {
        IUIAutomationElement *e = NULL;
        char name[128];
        if (FAILED(IUIAutomationElementArray_GetElement(found, i, &e)) || !e) continue;
        name_of(e, name, sizeof name);
        if (!want) {
            CONTROLTYPEID type = 0;
            IUIAutomationElement_get_CurrentControlType(e, &type);
            printf("child: %d %s\n", (int)type, name);
        } else if (!strcmp(name, want)) {
            return e;
        }
    }
    return NULL;
}

static void *pattern(IUIAutomationElement *e, PATTERNID id, REFIID iid) {
    IUnknown *unknown = NULL;
    void *p = NULL;
    if (!e || FAILED(IUIAutomationElement_GetCurrentPattern(e, id, &unknown)) || !unknown) return NULL;
    IUnknown_QueryInterface(unknown, iid, &p);
    IUnknown_Release(unknown);
    return p;
}

static void read_state(IUIAutomationElement *root, const char *phase) {
    enum ToggleState t = 0;
    BSTR value = NULL;
    double volume = -1;
    char text[128] = "";
    IUIAutomationTogglePattern *toggle = pattern(child(root, "Wrap lines"), UIA_TogglePatternId, &IID_IUIAutomationTogglePattern);
    IUIAutomationValuePattern *val = pattern(child(root, "Name"), UIA_ValuePatternId, &IID_IUIAutomationValuePattern);
    IUIAutomationRangeValuePattern *range = pattern(child(root, "Volume"), UIA_RangeValuePatternId, &IID_IUIAutomationRangeValuePattern);
    if (toggle) IUIAutomationTogglePattern_get_CurrentToggleState(toggle, &t);
    if (val && SUCCEEDED(IUIAutomationValuePattern_get_CurrentValue(val, &value)) && value)
        WideCharToMultiByte(CP_UTF8, 0, value, -1, text, sizeof text, NULL, NULL);
    if (range) IUIAutomationRangeValuePattern_get_CurrentValue(range, &volume);
    printf("%s: toggle %d, name %s, volume %g\n", phase, (int)t, text, volume);
}

static void dump(IUIAutomationElement *e, int depth) {
    IUIAutomationCondition *all;
    IUIAutomationElementArray *found;
    int n = 0;
    char name[256];
    CONTROLTYPEID type = 0;
    name_of(e, name, sizeof name);
    IUIAutomationElement_get_CurrentControlType(e, &type);
    printf("%*s%d %s\n", depth * 2, "", (int)type, name);
    if (depth > 12) return;
    IUIAutomation_CreateTrueCondition(uia, &all);
    if (FAILED(IUIAutomationElement_FindAll(e, TreeScope_Children, all, &found)) || !found) return;
    IUIAutomationElementArray_get_Length(found, &n);
    for (int i = 0; i < n; i++) {
        IUIAutomationElement *c = NULL;
        if (SUCCEEDED(IUIAutomationElementArray_GetElement(found, i, &c)) && c) dump(c, depth + 1);
    }
}

int main(int argc, char **argv) {
    HWND hwnd;
    IUIAutomationElement *root;
    char line[64], name[128];
    CoInitializeEx(NULL, COINIT_APARTMENTTHREADED);
    if (FAILED(CoCreateInstance(&CLSID_CUIAutomation, NULL, CLSCTX_INPROC_SERVER, &IID_IUIAutomation, (void **)&uia))) {
        printf("client: no UI Automation\n");
        return 1;
    }
    if (argc == 3 && !strcmp(argv[1], "--tree")) {
        wchar_t title[256];
        MultiByteToWideChar(CP_UTF8, 0, argv[2], -1, title, 256);
        hwnd = FindWindowW(NULL, title);
        if (!hwnd || FAILED(IUIAutomation_ElementFromHandle(uia, hwnd, &root))) {
            printf("client: no window titled %s\n", argv[2]);
            return 1;
        }
        dump(root, 0);
        return 0;
    }
    hwnd = FindWindowW(NULL, L"UIA probe");
    if (!hwnd || FAILED(IUIAutomation_ElementFromHandle(uia, hwnd, &root))) {
        printf("client: no probe window\n");
        return 1;
    }
    name_of(root, name, sizeof name);
    printf("root: %s\n", name);
    child(root, NULL);
    BOOL focus = FALSE;
    IUIAutomationElement_get_CurrentHasKeyboardFocus(child(root, "Save"), &focus);
    printf("save has focus: %d\n", focus ? 1 : 0);
    read_state(root, "before");
    IUIAutomationInvokePattern *invoke = pattern(child(root, "Save"), UIA_InvokePatternId, &IID_IUIAutomationInvokePattern);
    printf("invoke: %s\n", invoke && SUCCEEDED(IUIAutomationInvokePattern_Invoke(invoke)) ? "ok" : "failed");
    IUIAutomationTogglePattern *toggle = pattern(child(root, "Wrap lines"), UIA_TogglePatternId, &IID_IUIAutomationTogglePattern);
    printf("toggle: %s\n", toggle && SUCCEEDED(IUIAutomationTogglePattern_Toggle(toggle)) ? "ok" : "failed");
    IUIAutomationValuePattern *val = pattern(child(root, "Name"), UIA_ValuePatternId, &IID_IUIAutomationValuePattern);
    printf("set value: %s\n", val && SUCCEEDED(IUIAutomationValuePattern_SetValue(val, L"Grace")) ? "ok" : "failed");
    IUIAutomationRangeValuePattern *range = pattern(child(root, "Volume"), UIA_RangeValuePatternId, &IID_IUIAutomationRangeValuePattern);
    printf("set range: %s\n", range && SUCCEEDED(IUIAutomationRangeValuePattern_SetValue(range, 70.0)) ? "ok" : "failed");
    fflush(stdout);
    if (!fgets(line, sizeof line, stdin)) return 1;
    read_state(root, "after");
    printf("set focus: %s\n", SUCCEEDED(IUIAutomationElement_SetFocus(child(root, "Save"))) ? "ok" : "failed");
    fflush(stdout);
    return 0;
}
