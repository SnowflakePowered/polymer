#import <FSKit/FSKit.h>

NS_ASSUME_NONNULL_BEGIN
// Implemented and registered by objc2 in polymer-vfs-fskit.
@interface PolymerFileSystem : FSUnaryFileSystem <FSUnaryFileSystemOperations>
@end
extern PolymerFileSystem *PolymerCreateFileSystem(void) NS_RETURNS_RETAINED;
NS_ASSUME_NONNULL_END
