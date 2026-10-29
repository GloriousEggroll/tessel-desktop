/* GTK owns the display, event dispatch, surfaces, and commits. This bridge
 * only adds double-buffered protocol state; it never roundtrips or commits. */
#include <gtk/gtk.h>
#include <gdk/wayland/gdkwayland.h>
#include <wayland-client.h>
#include <math.h>
#include <string.h>
#include "ext-background-effect-v1-client.h"
#include "waydock-blur-v1-client.h"

typedef struct {
    GdkDisplay *display; /* borrowed: storage belongs to the display */
    struct wl_registry *registry;
    struct ext_background_effect_manager_v1 *effects;
    struct waydock_blur_manager_v1 *radii;
    uint32_t effects_name, radii_name, capabilities;
} Manager;

typedef struct {
    GdkSurface *surface;
    struct ext_background_effect_surface_v1 *effect;
    struct waydock_blur_surface_v1 *radius;
    int x, y, width, height, corners, strength;
    gboolean seen;
} Surface;

typedef struct { GPtrArray *surfaces; } Group;

static void capabilities(void *data, struct ext_background_effect_manager_v1 *object, uint32_t flags) {
    (void)object;
    ((Manager *)data)->capabilities = flags;
}
static const struct ext_background_effect_manager_v1_listener effect_listener = {capabilities};

static void global(void *data, struct wl_registry *registry, uint32_t name,
        const char *interface, uint32_t version) {
    (void)version;
    Manager *manager = data;
    if (!strcmp(interface, ext_background_effect_manager_v1_interface.name) && !manager->effects) {
        manager->effects_name = name;
        manager->effects = wl_registry_bind(registry, name, &ext_background_effect_manager_v1_interface, 1);
        ext_background_effect_manager_v1_add_listener(manager->effects, &effect_listener, manager);
    } else if (!strcmp(interface, waydock_blur_manager_v1_interface.name) && !manager->radii) {
        manager->radii_name = name;
        manager->radii = wl_registry_bind(registry, name, &waydock_blur_manager_v1_interface, 1);
    }
}
static void global_remove(void *data, struct wl_registry *registry, uint32_t name) {
    (void)registry;
    Manager *manager = data;
    if (name == manager->effects_name && manager->effects) {
        ext_background_effect_manager_v1_destroy(manager->effects);
        manager->effects = NULL;
        manager->capabilities = 0;
    }
    if (name == manager->radii_name && manager->radii) {
        waydock_blur_manager_v1_destroy(manager->radii);
        manager->radii = NULL;
    }
}
static const struct wl_registry_listener registry_listener = {global, global_remove};

static void manager_free(gpointer data) {
    Manager *manager = data;
    /* GDK has already disconnected its Wayland display when closed. */
    if (!gdk_display_is_closed(manager->display)) {
        if (manager->effects) ext_background_effect_manager_v1_destroy(manager->effects);
        if (manager->radii) waydock_blur_manager_v1_destroy(manager->radii);
        if (manager->registry) wl_registry_destroy(manager->registry);
    }
    g_free(manager);
}
static Manager *manager_for(GdkDisplay *display) {
    if (!GDK_IS_WAYLAND_DISPLAY(display) || gdk_display_is_closed(display)) return NULL;
    Manager *manager = g_object_get_data(G_OBJECT(display), "waydock-background-blur-manager");
    if (!manager) {
        manager = g_new0(Manager, 1);
        manager->display = display;
        manager->registry = wl_display_get_registry(gdk_wayland_display_get_wl_display(display));
        wl_registry_add_listener(manager->registry, &registry_listener, manager);
        g_object_set_data_full(G_OBJECT(display), "waydock-background-blur-manager", manager, manager_free);
        /* GDK's source dispatches these events asynchronously on its own queue. */
        gdk_display_flush(display);
    }
    return manager;
}

static void surface_free(gpointer data) {
    Surface *surface = data;
    if (g_object_get_data(G_OBJECT(surface->surface), "waydock-background-blur-owner") == surface)
        g_object_set_data(G_OBJECT(surface->surface), "waydock-background-blur-owner", NULL);
    if (!gdk_display_is_closed(gdk_surface_get_display(surface->surface))) {
        if (surface->effect) ext_background_effect_surface_v1_destroy(surface->effect);
        if (surface->radius) waydock_blur_surface_v1_destroy(surface->radius);
        if (!gdk_surface_is_destroyed(surface->surface)) gdk_surface_queue_render(surface->surface);
    }
    g_object_unref(surface->surface);
    g_free(surface);
}

void *waydock_blur_new(void) {
    Group *group = g_new0(Group, 1);
    group->surfaces = g_ptr_array_new_with_free_func(surface_free);
    return group;
}
void waydock_blur_clear(void *handle) {
    Group *group = handle;
    if (group) g_ptr_array_set_size(group->surfaces, 0);
}
void waydock_blur_free(void *handle) {
    Group *group = handle;
    if (!group) return;
    g_ptr_array_unref(group->surfaces);
    g_free(group);
}

/* Diagnostics for integration checks: requested logical geometry, never pixels
 * from other applications. The native connection remains entirely GTK-owned. */
int waydock_blur_geometry(void *handle, unsigned index, int *values) {
    Group *group = handle;
    if (!group || index >= group->surfaces->len || !values) return 0;
    Surface *surface = g_ptr_array_index(group->surfaces, index);
    int snapshot[] = {surface->x, surface->y, surface->width, surface->height,
                      surface->corners, surface->strength};
    memcpy(values, snapshot, sizeof(snapshot));
    return 1;
}

static GtkWidget *find_panel(GtkWidget *widget, const char *class_name) {
    if (class_name && *class_name && gtk_widget_has_css_class(widget, class_name)) return widget;
    for (GtkWidget *child = gtk_widget_get_first_child(widget); child; child = gtk_widget_get_next_sibling(child)) {
        if (GTK_IS_NATIVE(child)) continue;
        GtkWidget *found = find_panel(child, class_name);
        if (found) return found;
    }
    return NULL;
}

static struct wl_region *rounded_region(Manager *manager, int x, int y, int width, int height, int radius) {
    struct wl_region *region = wl_compositor_create_region(gdk_wayland_display_get_wl_compositor(manager->display));
    radius = MIN(radius, MIN(width, height) / 2);
    if (radius <= 0) {
        wl_region_add(region, x, y, width, height);
        return region;
    }
    /* Pixel-centre scanlines keep the effect inside the CSS rounded outline.
     * Full middle rectangle means cost is bounded by 2*radius, not panel size. */
    wl_region_add(region, x, y + radius, width, height - 2 * radius);
    for (int row = 0; row < radius; row++) {
        double dy = radius - row - .5;
        int inset = (int)ceil(radius - sqrt(radius * radius - dy * dy));
        if (width - inset * 2 > 0) {
            wl_region_add(region, x + inset, y + row, width - inset * 2, 1);
            wl_region_add(region, x + inset, y + height - row - 1, width - inset * 2, 1);
        }
    }
    return region;
}

static void apply_native(Group *group, GtkWidget *widget, GtkWidget *panel, int corners, int strength) {
    GdkSurface *native = gtk_native_get_surface(GTK_NATIVE(widget));
    if (!native || !GDK_IS_WAYLAND_SURFACE(native) || gdk_surface_is_destroyed(native)) return;
    Manager *manager = manager_for(gdk_surface_get_display(native));
    if (!manager || !manager->effects || !(manager->capabilities & EXT_BACKGROUND_EFFECT_MANAGER_V1_CAPABILITY_BLUR)) return;
    graphene_rect_t bounds;
    if (!gtk_widget_compute_bounds(panel, widget, &bounds)) return;
    double tx, ty;
    gtk_native_get_surface_transform(GTK_NATIVE(widget), &tx, &ty);
    /* GTK adds this translation when snapshotting the native widget into its
     * surface. In particular, CSD shadows put the window at a positive offset
     * inside the wl_surface. Subtracting it shifts blur into the top/left
     * shadow and leaves the bottom/right of the window unblurred. */
    double left = bounds.origin.x + tx, top = bounds.origin.y + ty;
    int x = (int)ceil(left), y = (int)ceil(top);
    int width = (int)floor(left + bounds.size.width) - x;
    int height = (int)floor(top + bounds.size.height) - y;
    if (width <= 0 || height <= 0) return;
    Surface *surface = NULL;
    for (guint i = 0; i < group->surfaces->len; i++) {
        Surface *candidate = g_ptr_array_index(group->surfaces, i);
        if (candidate->surface == native) { surface = candidate; break; }
    }
    if (!surface) {
        /* Independently themed child roots can overlap a parent's traversal.
         * Only one extension object is permitted per wl_surface. The current
         * owner wins until it releases that surface; never disconnect GTK. */
        if (g_object_get_data(G_OBJECT(native), "waydock-background-blur-owner")) return;
        struct wl_surface *wl_surface = gdk_wayland_surface_get_wl_surface(native);
        if (!wl_surface) return;
        surface = g_new0(Surface, 1);
        surface->surface = g_object_ref(native);
        surface->effect = ext_background_effect_manager_v1_get_background_effect(manager->effects, wl_surface);
        if (manager->radii) surface->radius = waydock_blur_manager_v1_get_surface(manager->radii, wl_surface);
        surface->strength = -1;
        g_object_set_data(G_OBJECT(native), "waydock-background-blur-owner", surface);
        g_ptr_array_add(group->surfaces, surface);
    }
    surface->seen = TRUE;
    if (surface->x == x && surface->y == y && surface->width == width && surface->height == height
            && surface->corners == corners && surface->strength == strength) return;
    struct wl_region *region = rounded_region(manager, x, y, width, height, corners);
    ext_background_effect_surface_v1_set_blur_region(surface->effect, region);
    wl_region_destroy(region);
    if (surface->radius) waydock_blur_surface_v1_set_radius(surface->radius, strength);
    surface->x = x; surface->y = y; surface->width = width; surface->height = height;
    surface->corners = corners; surface->strength = strength;
    gtk_widget_queue_draw(widget);
    gdk_surface_queue_render(native);
}

static void visit(Group *group, GtkWidget *widget, GtkWidget *root, const char *target_class,
        gboolean include_root, gboolean include_popovers, int corners, int strength) {
    if (!gtk_widget_get_mapped(widget)) return;
    if (GTK_IS_POPOVER(widget) && !include_popovers) return;
    if (GTK_IS_NATIVE(widget)) {
        GtkWidget *panel = NULL;
        if (widget == root && include_root) {
            panel = target_class && *target_class ? find_panel(widget, target_class) : widget;
        } else if (GTK_IS_POPOVER(widget)) {
            /* GtkPopoverContent owns the background; not the transparent
             * popup margin, shadow, or positioning arrow. */
            for (GtkWidget *child = gtk_widget_get_first_child(widget); child; child = gtk_widget_get_next_sibling(child)) {
                if (!strcmp(gtk_widget_get_css_name(child), "contents")) { panel = child; break; }
            }
        }
        if (panel) apply_native(group, widget, panel, corners, strength);
    }
    for (GtkWidget *child = gtk_widget_get_first_child(widget); child; child = gtk_widget_get_next_sibling(child))
        visit(group, child, root, target_class, include_root, include_popovers, corners, strength);
}

int waydock_blur_update(void *handle, GtkWidget *root, const char *target_class,
        int enabled, int include_root, int include_popovers, int corners, int strength) {
    Group *group = handle;
    if (!group || !GTK_IS_WIDGET(root)) return 0;
    for (guint i = 0; i < group->surfaces->len; i++) ((Surface *)g_ptr_array_index(group->surfaces, i))->seen = FALSE;
    if (enabled && strength > 0) visit(group, root, root, target_class, include_root, include_popovers, CLAMP(corners, 0, 64), CLAMP(strength, 0, 40));
    for (guint i = group->surfaces->len; i > 0; i--) {
        Surface *surface = g_ptr_array_index(group->surfaces, i - 1);
        if (!surface->seen) g_ptr_array_remove_index(group->surfaces, i - 1);
    }
    return (int)group->surfaces->len;
}
