#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <fcntl.h>
#import <unistd.h>

static NSString *const OrbiBridgeDirectoryEnv = @"ORBI_MACOS_UI_BRIDGE_DIR";
static NSString *const OrbiBridgeNotificationEnv = @"ORBI_MACOS_UI_BRIDGE_NOTIFICATION";
static NSString *const OrbiLogPipeEnv = @"ORBI_MACOS_UI_LOG_PIPE";
static NSString *const OrbiRunTraceLaunchIDEnv = @"ORBI_MACOS_UI_TRACE_LAUNCH_ID";
static NSString *const OrbiRunTraceRegistrationPathEnv = @"ORBI_MACOS_UI_TRACE_REGISTRATION_PATH";
static NSString *const OrbiRunTraceExpectedBundleIDEnv = @"ORBI_MACOS_UI_TRACE_EXPECTED_BUNDLE_ID";

@interface OrbiMacOSUIBridge : NSObject

@property(nonatomic, copy) NSString *bridgeDirectory;
@property(nonatomic, copy) NSString *notificationName;
@property(nonatomic, weak) NSView *lastHoveredView;
@property(nonatomic, weak) NSView *dragView;
@property(nonatomic) int logPipeFileDescriptor;

+ (instancetype)sharedBridge;
- (void)configureLogPipeIfConfigured;
- (void)startIfConfigured;
- (void)writeTraceLaunchRegistrationIfConfigured:
    (NSDictionary<NSString *, NSString *> *)environment;

@end

@implementation OrbiMacOSUIBridge

+ (void)load {
    [[self sharedBridge] configureLogPipeIfConfigured];
    dispatch_async(dispatch_get_main_queue(), ^{
        [[self sharedBridge] startIfConfigured];
    });
}

+ (instancetype)sharedBridge {
    static OrbiMacOSUIBridge *bridge = nil;
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        bridge = [[self alloc] init];
        bridge.logPipeFileDescriptor = -1;
    });
    return bridge;
}

- (void)dealloc {
    if (_logPipeFileDescriptor >= 0) {
        close(_logPipeFileDescriptor);
    }
}

- (void)configureLogPipeIfConfigured {
    if (self.logPipeFileDescriptor >= 0) {
        return;
    }

    NSString *logPipePath = NSProcessInfo.processInfo.environment[OrbiLogPipeEnv];
    if (logPipePath.length == 0) {
        return;
    }

    int fileDescriptor = open(logPipePath.fileSystemRepresentation, O_WRONLY | O_CLOEXEC);
    if (fileDescriptor < 0) {
        return;
    }

    self.logPipeFileDescriptor = fileDescriptor;
    dup2(fileDescriptor, STDOUT_FILENO);
    dup2(fileDescriptor, STDERR_FILENO);
    setvbuf(stdout, NULL, _IOLBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);
}

- (void)startIfConfigured {
    NSDictionary<NSString *, NSString *> *environment = NSProcessInfo.processInfo.environment;
    NSString *bridgeDirectory = environment[OrbiBridgeDirectoryEnv];
    NSString *notificationName = environment[OrbiBridgeNotificationEnv];
    if (bridgeDirectory.length == 0 || notificationName.length == 0) {
        return;
    }

    [self writeTraceLaunchRegistrationIfConfigured:environment];

    if ([self.notificationName isEqualToString:notificationName] &&
        [self.bridgeDirectory isEqualToString:bridgeDirectory]) {
        return;
    }

    self.bridgeDirectory = bridgeDirectory;
    self.notificationName = notificationName;

    NSDistributedNotificationCenter *center = NSDistributedNotificationCenter.defaultCenter;
    [center removeObserver:self name:nil object:nil];
    [center addObserver:self
               selector:@selector(handleBridgeNotification:)
                   name:self.notificationName
                 object:nil
     suspensionBehavior:NSNotificationSuspensionBehaviorDeliverImmediately];
}

- (void)writeTraceLaunchRegistrationIfConfigured:
    (NSDictionary<NSString *, NSString *> *)environment {
    NSString *launchID = environment[OrbiRunTraceLaunchIDEnv];
    NSString *registrationPath = environment[OrbiRunTraceRegistrationPathEnv];
    if (launchID.length == 0 || registrationPath.length == 0) {
        return;
    }

    NSString *bundleID = NSBundle.mainBundle.bundleIdentifier ?: @"";
    NSString *expectedBundleID = environment[OrbiRunTraceExpectedBundleIDEnv];
    if (expectedBundleID.length > 0 && ![bundleID isEqualToString:expectedBundleID]) {
        return;
    }

    NSDictionary *registration = @{
        @"pid" : @(NSProcessInfo.processInfo.processIdentifier),
        @"launchId" : launchID,
        @"bundleId" : bundleID,
    };

    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:registration options:0 error:&error];
    if (data == nil) {
        return;
    }

    NSString *parentDirectory = [registrationPath stringByDeletingLastPathComponent];
    if (parentDirectory.length > 0) {
        [NSFileManager.defaultManager createDirectoryAtPath:parentDirectory
                                withIntermediateDirectories:YES
                                                 attributes:nil
                                                      error:nil];
    }
    [data writeToFile:registrationPath options:NSDataWritingAtomic error:nil];
}

- (void)handleBridgeNotification:(NSNotification *)notification {
    NSString *requestID = notification.userInfo[@"requestId"];
    if (requestID.length == 0) {
        return;
    }

    NSString *requestPath =
        [self.bridgeDirectory stringByAppendingPathComponent:
                                  [NSString stringWithFormat:@"request-%@.json", requestID]];
    NSString *responsePath =
        [self.bridgeDirectory stringByAppendingPathComponent:
                                  [NSString stringWithFormat:@"response-%@.json", requestID]];

    dispatch_async(dispatch_get_main_queue(), ^{
        @autoreleasepool {
            [self processRequestAtPath:requestPath responsePath:responsePath];
        }
    });
}

- (void)processRequestAtPath:(NSString *)requestPath responsePath:(NSString *)responsePath {
    NSError *readError = nil;
    NSData *requestData = [NSData dataWithContentsOfFile:requestPath options:0 error:&readError];
    if (requestData == nil) {
        [self writeResponse:@{
            @"ok" : @NO,
            @"error" : readError.localizedDescription ?: @"failed to read bridge request"
        } toPath:responsePath];
        return;
    }

    id jsonObject = [NSJSONSerialization JSONObjectWithData:requestData options:0 error:&readError];
    if (![jsonObject isKindOfClass:[NSDictionary class]]) {
        [self writeResponse:@{
            @"ok" : @NO,
            @"error" : readError.localizedDescription ?: @"bridge request was not a JSON object"
        } toPath:responsePath];
        return;
    }

    NSError *commandError = nil;
    BOOL success = [self executeRequest:(NSDictionary *)jsonObject error:&commandError];
    [self writeResponse:@{
        @"ok" : @(success),
        @"error" : success ? [NSNull null]
                           : (commandError.localizedDescription ?: @"bridge command failed"),
    } toPath:responsePath];
}

- (void)writeResponse:(NSDictionary *)response toPath:(NSString *)responsePath {
    NSError *error = nil;
    NSData *responseData =
        [NSJSONSerialization dataWithJSONObject:response options:0 error:&error];
    if (responseData == nil) {
        return;
    }
    [responseData writeToFile:responsePath atomically:YES];
}

- (NSError *)bridgeError:(NSString *)description {
    return [NSError errorWithDomain:@"OrbiMacOSUIBridge"
                               code:1
                           userInfo:@{NSLocalizedDescriptionKey : description}];
}

- (NSNumber *)numberForKey:(NSString *)key
               inDictionary:(NSDictionary *)dictionary
                      error:(NSError **)error {
    id value = dictionary[key];
    if ([value isKindOfClass:[NSNumber class]]) {
        return value;
    }
    if (error != NULL) {
        *error = [self bridgeError:[NSString stringWithFormat:@"missing numeric `%@`", key]];
    }
    return nil;
}

- (NSString *)stringForKey:(NSString *)key
              inDictionary:(NSDictionary *)dictionary
                     error:(NSError **)error {
    id value = dictionary[key];
    if ([value isKindOfClass:[NSString class]] && [value length] > 0) {
        return value;
    }
    if (error != NULL) {
        *error = [self bridgeError:[NSString stringWithFormat:@"missing string `%@`", key]];
    }
    return nil;
}

- (NSArray<NSWindow *> *)orderedVisibleWindows {
    NSMutableArray<NSWindow *> *windows = [NSMutableArray array];
    for (NSWindow *window in NSApp.orderedWindows) {
        if (!window.visible || window.miniaturized) {
            continue;
        }
        [windows addObject:window];
    }
    return windows;
}

- (NSRect)automationFrameForWindow:(NSWindow *)window {
    NSScreen *screen = window.screen ?: NSScreen.mainScreen;
    if (screen == nil) {
        return NSZeroRect;
    }

    NSRect frame = window.frame;
    CGFloat topOriginY = NSMaxY(screen.frame) - NSMaxY(frame);
    return NSMakeRect(frame.origin.x, topOriginY, frame.size.width, frame.size.height);
}

- (NSWindow *)windowForAutomationPoint:(NSPoint)point {
    for (NSWindow *window in [self orderedVisibleWindows]) {
        if (NSPointInRect(point, [self automationFrameForWindow:window])) {
            return window;
        }
    }

    if (NSApp.keyWindow != nil) {
        return NSApp.keyWindow;
    }
    if (NSApp.mainWindow != nil) {
        return NSApp.mainWindow;
    }
    return [self orderedVisibleWindows].firstObject;
}

- (NSPoint)localPointForAutomationPoint:(NSPoint)point inWindow:(NSWindow *)window {
    NSScreen *screen = window.screen ?: NSScreen.mainScreen;
    if (screen == nil) {
        return point;
    }

    NSPoint screenPoint = NSMakePoint(point.x, NSMaxY(screen.frame) - point.y);
    return [window convertPointFromScreen:screenPoint];
}

- (NSView *)hitViewForAutomationPoint:(NSPoint)point
                             inWindow:(NSWindow **)windowOut
                           localPoint:(NSPoint *)localPointOut {
    NSWindow *window = [self windowForAutomationPoint:point];
    if (window == nil) {
        return nil;
    }

    NSPoint localPoint = [self localPointForAutomationPoint:point inWindow:window];
    if (windowOut != NULL) {
        *windowOut = window;
    }
    if (localPointOut != NULL) {
        *localPointOut = localPoint;
    }

    NSView *contentView = window.contentView;
    if (contentView == nil) {
        return nil;
    }

    NSPoint contentPoint = [contentView convertPoint:localPoint fromView:nil];
    NSView *hitView = [contentView hitTest:contentPoint] ?: contentView;
    NSPoint hitPoint = [hitView convertPoint:contentPoint fromView:contentView];
    return [self deepestDescendantInView:hitView containingPoint:hitPoint] ?: hitView;
}

- (id)accessibilityElementForAutomationPoint:(NSPoint)point inWindow:(NSWindow *)window {
    NSView *contentView = window.contentView;
    if (contentView == nil) {
        return nil;
    }

    NSScreen *screen = window.screen ?: NSScreen.mainScreen;
    if (screen == nil) {
        return nil;
    }

    NSPoint screenPoint = NSMakePoint(point.x, NSMaxY(screen.frame) - point.y);
    return [contentView accessibilityHitTest:screenPoint];
}

- (NSString *)stringAccessibilityAttribute:(NSString *)selectorName
                                 onElement:(id)element {
    SEL selector = NSSelectorFromString(selectorName);
    if (element == nil || ![element respondsToSelector:selector]) {
        return nil;
    }

    IMP implementation = [element methodForSelector:selector];
    NSString *(*function)(id, SEL) = (NSString *(*)(id, SEL))implementation;
    NSString *value = function(element, selector);
    return value.length > 0 ? value : nil;
}

- (NSString *)slugifiedStringCandidate:(NSString *)value {
    if (value.length == 0) {
        return nil;
    }

    NSCharacterSet *allowed = [NSCharacterSet alphanumericCharacterSet];
    NSMutableString *result = [NSMutableString string];
    BOOL previousWasSeparator = NO;
    for (NSUInteger index = 0; index < value.length; index += 1) {
        unichar character = [value characterAtIndex:index];
        if ([allowed characterIsMember:character]) {
            [result appendString:[[NSString stringWithCharacters:&character length:1] lowercaseString]];
            previousWasSeparator = NO;
        } else if (!previousWasSeparator) {
            [result appendString:@"-"];
            previousWasSeparator = YES;
        }
    }

    while ([result hasPrefix:@"-"]) {
        [result deleteCharactersInRange:NSMakeRange(0, 1)];
    }
    while ([result hasSuffix:@"-"]) {
        [result deleteCharactersInRange:NSMakeRange(result.length - 1, 1)];
    }
    return result.length > 0 ? result : nil;
}

- (NSArray<NSString *> *)textPayloadCandidatesForAutomationPoint:(NSPoint)point
                                                        inWindow:(NSWindow *)window {
    id accessibilityElement = [self accessibilityElementForAutomationPoint:point inWindow:window];
    NSMutableOrderedSet<NSString *> *candidates = [NSMutableOrderedSet orderedSet];
    NSArray<NSString *> *rawValues = @[
        [self stringAccessibilityAttribute:@"accessibilityValue" onElement:accessibilityElement] ?: @"",
        [self stringAccessibilityAttribute:@"accessibilityLabel" onElement:accessibilityElement] ?: @"",
        [self stringAccessibilityAttribute:@"accessibilityIdentifier" onElement:accessibilityElement] ?: @"",
    ];

    for (NSString *rawValue in rawValues) {
        NSString *trimmed = [rawValue stringByTrimmingCharactersInSet:
                                        [NSCharacterSet whitespaceAndNewlineCharacterSet]];
        if (trimmed.length == 0) {
            continue;
        }
        NSString *slug = [self slugifiedStringCandidate:trimmed];
        if (slug.length > 0) {
            [candidates addObject:slug];
        }
        [candidates addObject:trimmed.lowercaseString];
        [candidates addObject:trimmed];
    }

    return candidates.array;
}

- (id)draggingSourceForAutomationPoint:(NSPoint)point
                              inWindow:(NSWindow *)window {
    NSPoint localPoint = NSZeroPoint;
    NSView *view = [self hitViewForAutomationPoint:point inWindow:&window localPoint:&localPoint];
    while (view != nil) {
        if ([view conformsToProtocol:@protocol(NSDraggingSource)] ||
            [view respondsToSelector:
                      @selector(draggingSession:sourceOperationMaskForDraggingContext:)] ||
            [view respondsToSelector:@selector(draggingSourceOperationMaskForLocal:)]) {
            return view;
        }
        view = view.superview;
    }

    if ([window conformsToProtocol:@protocol(NSDraggingSource)] ||
        [window respondsToSelector:@selector(draggingSession:sourceOperationMaskForDraggingContext:)] ||
        [window respondsToSelector:@selector(draggingSourceOperationMaskForLocal:)]) {
        return window;
    }
    return window.contentView;
}

- (BOOL)performBackgroundDraggingSessionFromPoint:(NSPoint)sourcePoint
                                          toPoint:(NSPoint)destinationPoint
                                       durationMs:(int)durationMs
                                            delta:(NSInteger)delta
                                            error:(NSError **)error {
    NSWindow *sourceWindow = [self windowForAutomationPoint:sourcePoint];
    if (sourceWindow == nil) {
        return NO;
    }

    id draggingSource = [self draggingSourceForAutomationPoint:sourcePoint
                                                      inWindow:sourceWindow];
    NSView *sessionView = [draggingSource isKindOfClass:[NSView class]]
                              ? (NSView *)draggingSource
                              : sourceWindow.contentView;
    if (sessionView == nil) {
        return NO;
    }

    NSArray<NSString *> *payloadCandidates =
        [self textPayloadCandidatesForAutomationPoint:sourcePoint inWindow:sourceWindow];
    if (payloadCandidates.count == 0) {
        return NO;
    }

    CGFloat distance = hypot(destinationPoint.x - sourcePoint.x, destinationPoint.y - sourcePoint.y);
    NSInteger steps = MAX(2, (NSInteger)(distance / (CGFloat)MAX((NSInteger)1, delta)));
    useconds_t sleepMicros = (useconds_t)MAX(1, durationMs * 1000 / MAX((NSInteger)1, steps));

    BOOL dispatched = NO;
    for (NSString *payload in payloadCandidates) {
        NSDraggingItem *draggingItem = [[NSDraggingItem alloc] initWithPasteboardWriter:payload];
        NSPoint sourceLocation = [self localPointForAutomationPoint:sourcePoint inWindow:sourceWindow];
        NSPoint sourceViewPoint = [sessionView convertPoint:sourceLocation fromView:nil];
        NSRect sourceFrame = NSMakeRect(sourceViewPoint.x - 1, sourceViewPoint.y - 1, 2, 2);
        NSImage *dragImage = [[NSImage alloc] initWithSize:NSMakeSize(2, 2)];
        [draggingItem setDraggingFrame:sourceFrame contents:dragImage];

        NSEvent *downEvent = [NSEvent mouseEventWithType:NSEventTypeLeftMouseDown
                                                location:sourceLocation
                                           modifierFlags:0
                                               timestamp:NSProcessInfo.processInfo.systemUptime
                                            windowNumber:sourceWindow.windowNumber
                                                 context:nil
                                             eventNumber:0
                                              clickCount:1
                                                pressure:1];
        if (downEvent == nil) {
            continue;
        }

        __block NSError *dispatchError = nil;
        __block BOOL dispatchCompleted = NO;
        dispatch_semaphore_t dispatchCompletedSignal = dispatch_semaphore_create(0);
        CGEventSourceRef eventSource = [self newMouseEventSource];
        if (eventSource == NULL) {
            if (error != NULL) {
                *error = [self bridgeError:@"failed to construct a Quartz event source for the drag session"];
            }
            return dispatched;
        }
        BOOL downSuccess = [self postCGMouseEventOfType:kCGEventMouseMoved
                                        automationPoint:sourcePoint
                                               inWindow:sourceWindow
                                                 source:eventSource
                                             clickCount:0
                                                 button:kCGMouseButtonLeft
                                                  delta:NSZeroPoint
                                               pressure:0
                                        routeToProcess:YES
                                                  error:&dispatchError];
        if (downSuccess) {
            downSuccess = [self postCGMouseEventOfType:kCGEventLeftMouseDown
                                       automationPoint:sourcePoint
                                              inWindow:sourceWindow
                                                source:eventSource
                                            clickCount:1
                                                button:kCGMouseButtonLeft
                                                 delta:NSZeroPoint
                                              pressure:1
                                       routeToProcess:YES
                                                 error:&dispatchError];
        }
        if (!downSuccess) {
            CFRelease(eventSource);
            if (error != NULL && dispatchError != nil) {
                *error = dispatchError;
            }
            continue;
        }
        CGEventSourceRef capturedSource = eventSource;
        CFRetain(capturedSource);
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
                usleep(40 * 1000);
                NSPoint previousPoint = sourcePoint;
                BOOL success = YES;
                for (NSInteger step = 1; success && step <= steps; step += 1) {
                    CGFloat progress = (CGFloat)step / (CGFloat)steps;
                    NSPoint point = NSMakePoint(
                        sourcePoint.x + ((destinationPoint.x - sourcePoint.x) * progress),
                        sourcePoint.y + ((destinationPoint.y - sourcePoint.y) * progress));
                    NSPoint deltaPoint = NSMakePoint(point.x - previousPoint.x, point.y - previousPoint.y);
                    NSWindow *window = [self windowForAutomationPoint:point] ?: sourceWindow;
                    success = [self postCGMouseEventOfType:kCGEventLeftMouseDragged
                                           automationPoint:point
                                                  inWindow:window
                                                    source:capturedSource
                                                clickCount:1
                                                    button:kCGMouseButtonLeft
                                                     delta:deltaPoint
                                                  pressure:1
                                           routeToProcess:YES
                                                     error:&dispatchError];
                    previousPoint = point;
                    [[NSRunLoop currentRunLoop]
                        runUntilDate:[NSDate dateWithTimeIntervalSinceNow:((double)sleepMicros) / 1e6]];
                }

                if (success) {
                    NSWindow *destinationWindow =
                        [self windowForAutomationPoint:destinationPoint] ?: sourceWindow;
                    success = [self postCGMouseEventOfType:kCGEventLeftMouseUp
                                           automationPoint:destinationPoint
                                                  inWindow:destinationWindow
                                                    source:capturedSource
                                                clickCount:1
                                                    button:kCGMouseButtonLeft
                                                     delta:NSZeroPoint
                                                  pressure:0
                                           routeToProcess:YES
                                                     error:&dispatchError];
                }

                usleep(80 * 1000);
                dispatchCompleted = YES;
                dispatch_semaphore_signal(dispatchCompletedSignal);
                CFRelease(capturedSource);
            });
        NSDraggingSession *session =
            [sessionView beginDraggingSessionWithItems:@[ draggingItem ]
                                                 event:downEvent
                                                source:draggingSource];
        CFRelease(eventSource);
        if (session == nil) {
            continue;
        }

        session.animatesToStartingPositionsOnCancelOrFail = NO;
        session.draggingFormation = NSDraggingFormationDefault;
        dispatched = YES;
        long waitResult = dispatch_semaphore_wait(
            dispatchCompletedSignal,
            dispatch_time(DISPATCH_TIME_NOW,
                          (int64_t)((MAX(1.0, ((double)durationMs) / 1000.0 + 0.5)) * NSEC_PER_SEC)));
        if (waitResult != 0) {
            dispatchError = [self bridgeError:@"timed out waiting for the background drag events to complete"];
        }
        if (!dispatchCompleted && dispatchError == nil) {
            dispatchError = [self bridgeError:@"timed out waiting for the background drag events to complete"];
        }
        if (dispatchError == nil) {
            return YES;
        }
        if (error != NULL) {
            *error = dispatchError;
        }
    }

    if (!dispatched && error != NULL) {
        *error = [self bridgeError:@"failed to begin a background AppKit drag session"];
    }
    return dispatched;
}

- (NSView *)deepestDescendantInView:(NSView *)view containingPoint:(NSPoint)point {
    for (NSView *subview in view.subviews.reverseObjectEnumerator) {
        if (subview.hidden || subview.alphaValue <= 0.01) {
            continue;
        }

        NSPoint subviewPoint = [subview convertPoint:point fromView:view];
        if (!NSPointInRect(subviewPoint, subview.bounds)) {
            continue;
        }

        NSView *deeper = [self deepestDescendantInView:subview containingPoint:subviewPoint];
        return deeper ?: subview;
    }

    return view;
}

- (NSView *)hoverRecipientForView:(NSView *)view {
    NSView *candidate = view;
    while (candidate != nil) {
        [candidate updateTrackingAreas];
        if ([candidate respondsToSelector:@selector(mouseEntered:)] ||
            [candidate respondsToSelector:@selector(mouseMoved:)] ||
            [candidate respondsToSelector:@selector(mouseExited:)]) {
            return candidate;
        }
        candidate = candidate.superview;
    }
    return view;
}

- (id)scrollRecipientForView:(NSView *)view {
    if (view.enclosingScrollView != nil) {
        return view.enclosingScrollView;
    }

    NSView *candidate = view;
    while (candidate != nil) {
        if ([candidate respondsToSelector:@selector(scrollWheel:)]) {
            return candidate;
        }
        candidate = candidate.superview;
    }

    return nil;
}

- (void)dispatchViewFallbackForMouseEvent:(NSEvent *)event
                                 ofType:(NSEventType)type
                         automationPoint:(NSPoint)point {
    NSWindow *window = nil;
    NSPoint localPoint = NSZeroPoint;
    NSView *view = [self hitViewForAutomationPoint:point inWindow:&window localPoint:&localPoint];
    if (view == nil) {
        return;
    }

    switch (type) {
        case NSEventTypeMouseMoved: {
            NSView *hoverView = [self hoverRecipientForView:view];
            if (hoverView != self.lastHoveredView) {
                if (self.lastHoveredView != nil &&
                    [self.lastHoveredView respondsToSelector:@selector(mouseExited:)]) {
                    NSEvent *exited = [NSEvent enterExitEventWithType:NSEventTypeMouseExited
                                                             location:localPoint
                                                        modifierFlags:0
                                                            timestamp:NSProcessInfo.processInfo.systemUptime
                                                         windowNumber:window.windowNumber
                                                              context:nil
                                                          eventNumber:0
                                                       trackingNumber:0
                                                             userData:NULL];
                    if (exited != nil) {
                        [self.lastHoveredView mouseExited:exited];
                    }
                }
                if ([hoverView respondsToSelector:@selector(mouseEntered:)]) {
                    NSEvent *entered = [NSEvent enterExitEventWithType:NSEventTypeMouseEntered
                                                              location:localPoint
                                                         modifierFlags:0
                                                             timestamp:NSProcessInfo.processInfo.systemUptime
                                                          windowNumber:window.windowNumber
                                                               context:nil
                                                           eventNumber:0
                                                        trackingNumber:0
                                                              userData:NULL];
                    if (entered != nil) {
                        [hoverView mouseEntered:entered];
                    }
                }
                self.lastHoveredView = hoverView;
            }
            if ([hoverView respondsToSelector:@selector(mouseMoved:)]) {
                [hoverView mouseMoved:event];
            }
            break;
        }
        case NSEventTypeRightMouseDown:
            if ([view respondsToSelector:@selector(rightMouseDown:)]) {
                [view rightMouseDown:event];
            }
            break;
        case NSEventTypeRightMouseUp:
            if ([view respondsToSelector:@selector(rightMouseUp:)]) {
                [view rightMouseUp:event];
            }
            break;
        case NSEventTypeLeftMouseDown:
            self.dragView = view;
            if ([view respondsToSelector:@selector(mouseDown:)]) {
                [view mouseDown:event];
            }
            break;
        case NSEventTypeLeftMouseDragged: {
            NSView *dragView = self.dragView ?: view;
            if ([dragView respondsToSelector:@selector(mouseDragged:)]) {
                [dragView mouseDragged:event];
            }
            break;
        }
        case NSEventTypeLeftMouseUp: {
            NSView *dragView = self.dragView ?: view;
            if ([dragView respondsToSelector:@selector(mouseUp:)]) {
                [dragView mouseUp:event];
            }
            self.dragView = nil;
            break;
        }
        default:
            break;
    }
}

- (BOOL)dispatchMouseEventOfType:(NSEventType)type
                    clickCount:(NSInteger)clickCount
                automationPoint:(NSPoint)point
                          error:(NSError **)error {
    NSWindow *window = [self windowForAutomationPoint:point];
    if (window == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to resolve a target window for the pointer event"];
        }
        return NO;
    }

    NSPoint localPoint = [self localPointForAutomationPoint:point inWindow:window];
    NSEvent *event = [NSEvent mouseEventWithType:type
                                        location:localPoint
                                   modifierFlags:0
                                       timestamp:NSProcessInfo.processInfo.systemUptime
                                    windowNumber:window.windowNumber
                                         context:nil
                                     eventNumber:0
                                      clickCount:clickCount
                                        pressure:0];
    if (event == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to construct a mouse event"];
        }
        return NO;
    }

    [window sendEvent:event];
    [self dispatchViewFallbackForMouseEvent:event ofType:type automationPoint:point];
    return YES;
}

- (CGEventSourceRef)newMouseEventSource {
    CGEventSourceRef source = CGEventSourceCreate(kCGEventSourceStateCombinedSessionState);
    if (source != NULL) {
        CGEventSourceSetLocalEventsSuppressionInterval(source, 0);
    }
    return source;
}

- (NSEvent *)cgMouseEventOfType:(CGEventType)type
                automationPoint:(NSPoint)point
                       inWindow:(NSWindow *)window
                         source:(CGEventSourceRef)source
                     clickCount:(NSInteger)clickCount
                         button:(CGMouseButton)button
                          delta:(NSPoint)delta
                       pressure:(double)pressure {
    NSScreen *screen = window.screen ?: NSScreen.mainScreen;
    if (screen == nil) {
        return nil;
    }

    NSPoint screenPoint = NSMakePoint(point.x, NSMaxY(screen.frame) - point.y);
    CGEventRef cgEvent = CGEventCreateMouseEvent(
        source, type, CGPointMake(screenPoint.x, screenPoint.y), button);
    if (cgEvent == NULL) {
        return nil;
    }

    CGEventSetIntegerValueField(cgEvent, kCGMouseEventClickState, clickCount);
    CGEventSetIntegerValueField(cgEvent, kCGMouseEventButtonNumber, button);
    CGEventSetDoubleValueField(cgEvent, kCGMouseEventPressure, pressure);
    CGEventSetIntegerValueField(cgEvent, kCGMouseEventDeltaX, llround(delta.x));
    CGEventSetIntegerValueField(cgEvent, kCGMouseEventDeltaY, llround(delta.y));
    CGEventSetIntegerValueField(
        cgEvent, kCGMouseEventWindowUnderMousePointer, window.windowNumber);
    CGEventSetIntegerValueField(
        cgEvent,
        kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent,
        window.windowNumber);

    NSEvent *event = [NSEvent eventWithCGEvent:cgEvent];
    CFRelease(cgEvent);
    return event;
}

- (BOOL)postCGMouseEventOfType:(CGEventType)type
               automationPoint:(NSPoint)point
                      inWindow:(NSWindow *)window
                        source:(CGEventSourceRef)source
                    clickCount:(NSInteger)clickCount
                        button:(CGMouseButton)button
                         delta:(NSPoint)delta
                      pressure:(double)pressure
                routeToProcess:(BOOL)routeToProcess
                         error:(NSError **)error {
    NSScreen *screen = window.screen ?: NSScreen.mainScreen;
    if (screen == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to resolve a screen for the CG-backed mouse event"];
        }
        return NO;
    }

    NSPoint screenPoint = NSMakePoint(point.x, NSMaxY(screen.frame) - point.y);
    CGEventRef cgEvent = CGEventCreateMouseEvent(
        source, type, CGPointMake(screenPoint.x, screenPoint.y), button);
    if (cgEvent == NULL) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to construct a CG-backed mouse event"];
        }
        return NO;
    }

    CGEventSetIntegerValueField(cgEvent, kCGMouseEventClickState, clickCount);
    CGEventSetIntegerValueField(cgEvent, kCGMouseEventButtonNumber, button);
    CGEventSetDoubleValueField(cgEvent, kCGMouseEventPressure, pressure);
    CGEventSetIntegerValueField(cgEvent, kCGMouseEventDeltaX, llround(delta.x));
    CGEventSetIntegerValueField(cgEvent, kCGMouseEventDeltaY, llround(delta.y));
    CGEventSetIntegerValueField(
        cgEvent, kCGMouseEventWindowUnderMousePointer, window.windowNumber);
    CGEventSetIntegerValueField(
        cgEvent,
        kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent,
        window.windowNumber);

    if (routeToProcess) {
        CGEventPost(kCGHIDEventTap, cgEvent);
        CFRelease(cgEvent);
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.001]];
        return YES;
    }

    NSEvent *event = [NSEvent eventWithCGEvent:cgEvent];
    CFRelease(cgEvent);
    if (event == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to convert a CG-backed mouse event"];
        }
        return NO;
    }

    [NSApp postEvent:event atStart:NO];
    [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.001]];
    return YES;
}

- (BOOL)dispatchScrollWithVertical:(int32_t)vertical
                       horizontal:(int32_t)horizontal
                  automationPoint:(NSPoint)point
                            error:(NSError **)error {
    NSWindow *window = [self windowForAutomationPoint:point];
    if (window == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to resolve a target window for the scroll event"];
        }
        return NO;
    }

    NSScreen *screen = window.screen ?: NSScreen.mainScreen;
    if (screen == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to resolve a screen for the scroll event"];
        }
        return NO;
    }

    NSPoint screenPoint = NSMakePoint(point.x, NSMaxY(screen.frame) - point.y);
    CGEventRef cgEvent = CGEventCreateScrollWheelEvent(
        NULL, kCGScrollEventUnitPixel, 2, vertical, horizontal, 0);
    if (cgEvent == NULL) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to construct a scroll event"];
        }
        return NO;
    }

    CGEventSetLocation(cgEvent, CGPointMake(screenPoint.x, screenPoint.y));
    NSEvent *event = [NSEvent eventWithCGEvent:cgEvent];
    CFRelease(cgEvent);

    if (event == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to convert the scroll event"];
        }
        return NO;
    }

    [window sendEvent:event];
    NSWindow *hitWindow = nil;
    NSPoint localPoint = NSZeroPoint;
    NSView *view = [self hitViewForAutomationPoint:point inWindow:&hitWindow localPoint:&localPoint];
    id scrollRecipient = view != nil ? [self scrollRecipientForView:view] : nil;
    if ([scrollRecipient respondsToSelector:@selector(scrollWheel:)]) {
        [scrollRecipient scrollWheel:event];
    }
    return YES;
}

- (BOOL)executeTapRequest:(NSDictionary *)request error:(NSError **)error {
    NSNumber *x = [self numberForKey:@"x" inDictionary:request error:error];
    NSNumber *y = [self numberForKey:@"y" inDictionary:request error:error];
    if (x == nil || y == nil) {
        return NO;
    }

    NSNumber *durationMs = request[@"durationMs"];
    NSPoint point = NSMakePoint(x.doubleValue, y.doubleValue);
    if (![self dispatchMouseEventOfType:NSEventTypeMouseMoved
                             clickCount:0
                         automationPoint:point
                                   error:error]) {
        return NO;
    }
    if (![self dispatchMouseEventOfType:NSEventTypeLeftMouseDown
                             clickCount:1
                         automationPoint:point
                                   error:error]) {
        return NO;
    }
    if ([durationMs isKindOfClass:[NSNumber class]] && durationMs.intValue > 0) {
        usleep((useconds_t)(durationMs.intValue * 1000));
    }
    return [self dispatchMouseEventOfType:NSEventTypeLeftMouseUp
                               clickCount:1
                           automationPoint:point
                                     error:error];
}

- (BOOL)executeMoveRequest:(NSDictionary *)request error:(NSError **)error {
    NSNumber *x = [self numberForKey:@"x" inDictionary:request error:error];
    NSNumber *y = [self numberForKey:@"y" inDictionary:request error:error];
    if (x == nil || y == nil) {
        return NO;
    }
    return [self dispatchMouseEventOfType:NSEventTypeMouseMoved
                               clickCount:0
                           automationPoint:NSMakePoint(x.doubleValue, y.doubleValue)
                                     error:error];
}

- (BOOL)executeRightClickRequest:(NSDictionary *)request error:(NSError **)error {
    NSNumber *x = [self numberForKey:@"x" inDictionary:request error:error];
    NSNumber *y = [self numberForKey:@"y" inDictionary:request error:error];
    if (x == nil || y == nil) {
        return NO;
    }

    NSPoint point = NSMakePoint(x.doubleValue, y.doubleValue);
    if (![self dispatchMouseEventOfType:NSEventTypeMouseMoved
                             clickCount:0
                         automationPoint:point
                                   error:error]) {
        return NO;
    }
    if (![self dispatchMouseEventOfType:NSEventTypeRightMouseDown
                             clickCount:1
                         automationPoint:point
                                   error:error]) {
        return NO;
    }
    usleep(40 * 1000);
    return [self dispatchMouseEventOfType:NSEventTypeRightMouseUp
                               clickCount:1
                           automationPoint:point
                                     error:error];
}

- (BOOL)executeDragRequest:(NSDictionary *)request error:(NSError **)error {
    NSNumber *startX = [self numberForKey:@"startX" inDictionary:request error:error];
    NSNumber *startY = [self numberForKey:@"startY" inDictionary:request error:error];
    NSNumber *endX = [self numberForKey:@"endX" inDictionary:request error:error];
    NSNumber *endY = [self numberForKey:@"endY" inDictionary:request error:error];
    if (startX == nil || startY == nil || endX == nil || endY == nil) {
        return NO;
    }

    int durationMs = [request[@"durationMs"] isKindOfClass:[NSNumber class]]
                         ? [request[@"durationMs"] intValue]
                         : 650;
    NSInteger delta = [request[@"delta"] isKindOfClass:[NSNumber class]]
                          ? [request[@"delta"] integerValue]
                          : 6;
    delta = MAX(1, delta);

    NSPoint startPoint = NSMakePoint(startX.doubleValue, startY.doubleValue);
    NSPoint endPoint = NSMakePoint(endX.doubleValue, endY.doubleValue);
    CGFloat distance = hypot(endPoint.x - startPoint.x, endPoint.y - startPoint.y);
    NSInteger steps = MAX(2, (NSInteger)(distance / (CGFloat)delta));
    useconds_t sleepMicros = (useconds_t)MAX(1, durationMs * 1000 / MAX(1, steps));

    if (NSApp.isActive) {
        NSError *dragSessionError = nil;
        if ([self performBackgroundDraggingSessionFromPoint:startPoint
                                                    toPoint:endPoint
                                                 durationMs:durationMs
                                                      delta:delta
                                                      error:&dragSessionError]) {
            return YES;
        }
        if (error != NULL && dragSessionError != nil) {
            *error = dragSessionError;
        }
    }

    NSWindow *window = [self windowForAutomationPoint:startPoint];
    if (window == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to resolve a source window for the drag"];
        }
        return NO;
    }
    NSWindow *hitWindow = nil;
    NSPoint localPoint = NSZeroPoint;
    NSView *dragView = [self hitViewForAutomationPoint:startPoint
                                              inWindow:&hitWindow
                                            localPoint:&localPoint];
    if (dragView == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to resolve a source view for the drag"];
        }
        return NO;
    }

    // SwiftUI/AppKit drag-and-drop needs a coherent Quartz event source and
    // run-loop turns between mouse phases; synthetic NSEvents alone don't
    // reliably start a real dragging session when the AUT stays in background.
    CGEventSourceRef source = [self newMouseEventSource];
    if (source == NULL) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to construct a Quartz event source for drag"];
        }
        return NO;
    }

    NSEvent *moveEvent = [self cgMouseEventOfType:kCGEventMouseMoved
                                  automationPoint:startPoint
                                         inWindow:window
                                           source:source
                                       clickCount:0
                                           button:kCGMouseButtonLeft
                                            delta:NSZeroPoint
                                         pressure:0];
    if (moveEvent != nil && [dragView respondsToSelector:@selector(mouseMoved:)]) {
        [dragView mouseMoved:moveEvent];
    }
    [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.04]];

    NSEvent *downEvent = [self cgMouseEventOfType:kCGEventLeftMouseDown
                                   automationPoint:startPoint
                                          inWindow:window
                                            source:source
                                        clickCount:1
                                            button:kCGMouseButtonLeft
                                             delta:NSZeroPoint
                                          pressure:1];
    if (downEvent == nil) {
        if (error != NULL) {
            *error = [self bridgeError:@"failed to construct a mouse-down event for drag"];
        }
        CFRelease(source);
        return NO;
    }
    [dragView mouseDown:downEvent];
    [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.12]];

    BOOL success = YES;
    NSPoint previousPoint = startPoint;
    for (NSInteger step = 1; success && step <= steps; step += 1) {
        CGFloat progress = (CGFloat)step / (CGFloat)steps;
        NSPoint point = NSMakePoint(
            startPoint.x + ((endPoint.x - startPoint.x) * progress),
            startPoint.y + ((endPoint.y - startPoint.y) * progress));
        NSPoint deltaPoint = NSMakePoint(point.x - previousPoint.x, point.y - previousPoint.y);
        NSEvent *dragEvent = [self cgMouseEventOfType:kCGEventLeftMouseDragged
                                      automationPoint:point
                                             inWindow:window
                                               source:source
                                           clickCount:1
                                               button:kCGMouseButtonLeft
                                                delta:deltaPoint
                                             pressure:1];
        if (dragEvent == nil) {
            if (error != NULL) {
                *error = [self bridgeError:@"failed to construct a dragged event"];
            }
            success = NO;
            break;
        }
        [dragView mouseDragged:dragEvent];
        previousPoint = point;
        [[NSRunLoop currentRunLoop]
            runUntilDate:[NSDate dateWithTimeIntervalSinceNow:((double)sleepMicros) / 1e6]];
    }

    if (success) {
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.08]];
        NSEvent *upEvent = [self cgMouseEventOfType:kCGEventLeftMouseUp
                                    automationPoint:endPoint
                                           inWindow:window
                                             source:source
                                         clickCount:1
                                             button:kCGMouseButtonLeft
                                              delta:NSZeroPoint
                                           pressure:0];
        if (upEvent == nil) {
            if (error != NULL) {
                *error = [self bridgeError:@"failed to construct a mouse-up event for drag"];
            }
            success = NO;
        } else {
            [dragView mouseUp:upEvent];
        }
    }
    if (success) {
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.08]];
    }

    CFRelease(source);
    return success;
}

- (BOOL)executeScrollRequest:(NSDictionary *)request error:(NSError **)error {
    NSNumber *x = [self numberForKey:@"x" inDictionary:request error:error];
    NSNumber *y = [self numberForKey:@"y" inDictionary:request error:error];
    NSString *direction = [self stringForKey:@"direction" inDictionary:request error:error];
    if (x == nil || y == nil || direction == nil) {
        return NO;
    }

    int32_t vertical = 0;
    int32_t horizontal = 0;
    NSString *normalized = direction.lowercaseString;
    if ([normalized isEqualToString:@"up"]) {
        vertical = 500;
    } else if ([normalized isEqualToString:@"down"]) {
        vertical = -500;
    } else if ([normalized isEqualToString:@"left"]) {
        horizontal = -500;
    } else if ([normalized isEqualToString:@"right"]) {
        horizontal = 500;
    } else {
        if (error != NULL) {
            *error = [self bridgeError:
                [NSString stringWithFormat:@"unsupported scroll direction `%@`", direction]];
        }
        return NO;
    }

    return [self dispatchScrollWithVertical:vertical
                                  horizontal:horizontal
                             automationPoint:NSMakePoint(x.doubleValue, y.doubleValue)
                                       error:error];
}

- (BOOL)executeRequest:(NSDictionary *)request error:(NSError **)error {
    NSString *command = [self stringForKey:@"command" inDictionary:request error:error];
    if (command == nil) {
        return NO;
    }

    if ([command isEqualToString:@"ping"]) {
        return YES;
    }
    if ([command isEqualToString:@"tap"]) {
        return [self executeTapRequest:request error:error];
    }
    if ([command isEqualToString:@"move"]) {
        return [self executeMoveRequest:request error:error];
    }
    if ([command isEqualToString:@"right-click"]) {
        return [self executeRightClickRequest:request error:error];
    }
    if ([command isEqualToString:@"drag"]) {
        return [self executeDragRequest:request error:error];
    }
    if ([command isEqualToString:@"scroll"]) {
        return [self executeScrollRequest:request error:error];
    }

    if (error != NULL) {
        *error = [self bridgeError:
            [NSString stringWithFormat:@"unsupported bridge command `%@`", command]];
    }
    return NO;
}

@end
